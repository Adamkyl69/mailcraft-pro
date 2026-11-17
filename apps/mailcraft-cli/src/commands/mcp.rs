//! `mailcraft-cli mcp`: the bridge a local AI app (Claude Desktop, Claude Code, any MCP client on
//! this computer) starts. It reads JSON-RPC messages, one per line, on standard input and relays
//! each to the running MailCraft over its single-instance socket, one connection a message, and
//! writes the answer, one line, on standard output. Nothing else is ever written there;
//! diagnostics go to standard error.
//!
//! It runs before the data folder's store is opened (the app holds it), needs no account and
//! reads no mail itself: the token it is given (`MAILCRAFT_MCP_TOKEN`) is checked by MailCraft.

use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use mailcraft_engine::mcp::protocol::{self, INTERNAL_ERROR, INVALID_REQUEST, Id, NOT_RUNNING, PARSE_ERROR, Parsed, TOKEN_REFUSED};
use mailcraft_engine::mcp::{MAX_LINE, tokens};
use mailcraft_net::instance::{Patient, TIMEOUT, UnixStream, read_key, socket_path};

use super::{Args, Value, json};

/// Longest answer relayed, bytes (a message's text is at most 200 000 characters; a list of 200).
const MAX_REPLY: u64 = 8 << 20;
/// How long the bridge waits for MailCraft's answer: a little over what MailCraft's own socket
/// thread waits for its window (150 s).
const REPLY_WAIT: Duration = Duration::from_secs(160);
/// How often the key is looked for again, and how long between: MailCraft listens a moment
/// before its key has its name.
const KEY_TRIES: usize = 5;
const KEY_WAIT: Duration = Duration::from_millis(40);

const NOT_RUNNING_TEXT: &str = "MailCraft isn't running on this screen with this data folder: start it there, then try again \
     (the display or Windows session, and --data, pick which MailCraft the bridge reaches)";
const NO_SOCKET_TEXT: &str = "MailCraft can't be reached on this computer (no private folder for its socket)";
const TOKEN_TEXT: &str = "set MAILCRAFT_MCP_TOKEN to a token from MailCraft › File › Options › Connected apps";
const REFUSED_TEXT: &str = "MailCraft refused the request: is it a version with MCP, on the same data folder (--data)?";
const SLOW_TEXT: &str = "MailCraft didn't answer in time";

/// `mailcraft-cli mcp [--data DIR]`: the bridge between this process's standard input and output
/// and the running MailCraft. Ends with success when standard input closes.
pub(crate) fn bridge_main(args: &Args) -> Result<(), String> {
    let root = args.opt("--data").map(PathBuf::from).or_else(mailcraft_engine::store::default_root);
    let socket = root.as_deref().and_then(socket_path);
    let token = std::env::var("MAILCRAFT_MCP_TOKEN").ok();
    if !token.as_deref().is_some_and(tokens::looks_like_token) {
        errln!("mailcraft-cli mcp: {TOKEN_TEXT}");
    }
    let connect = || match &socket {
        Some(path) => connect_to(path),
        None => Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "no private folder for the socket")),
    };
    match bridge(std::io::stdin().lock(), std::io::stdout().lock(), token.as_deref(), connect) {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(format!("the MCP bridge stopped: {e}")),
        _ => Ok(()),
    }
}

/// A connection to the MailCraft listening at `socket`, and its key.
fn connect_to(socket: &Path) -> std::io::Result<(UnixStream, String)> {
    let stream = UnixStream::connect(socket)?;
    let mut tried = 1;
    loop {
        match read_key(socket) {
            Ok(key) if !key.is_empty() => return Ok((stream, key)),
            Ok(_) if tried >= KEY_TRIES => return Err(std::io::Error::other("no key")),
            Err(e) if tried >= KEY_TRIES => return Err(e),
            _ => {
                tried += 1;
                std::thread::sleep(KEY_WAIT);
            }
        }
    }
}

/// The bridge's loop: each line of `input` answered on `output`, one request at a time (the next
/// line is read once this one's answer is written). `connect` opens a connection to MailCraft, or
/// says why it can't. Ends with `Ok` when `input` ends (a last line without its newline is
/// dropped: it may be cut short).
pub(crate) fn bridge(
    mut input: impl BufRead,
    mut output: impl Write,
    token: Option<&str>,
    mut connect: impl FnMut() -> std::io::Result<(UnixStream, String)>,
) -> std::io::Result<()> {
    loop {
        let bytes = match read_capped(&mut input, MAX_LINE)? {
            Line::Eof => return Ok(()),
            Line::TooLong => {
                say(&mut output, &protocol::error(None, INVALID_REQUEST, "that line is longer than MailCraft reads (1 MiB)"))?;
                continue;
            }
            Line::Text(bytes) => bytes,
        };
        let Ok(text) = String::from_utf8(bytes) else {
            say(&mut output, &protocol::error(None, PARSE_ERROR, "that isn't UTF-8 text"))?;
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        // Parsed here only to learn the id (a notification has none and gets no answer, from
        // MailCraft either) and to refuse what MailCraft would.
        let id = match protocol::parse_line(&text) {
            Parsed::Invalid { id, code, message } => {
                say(&mut output, &protocol::error(id.as_ref(), code, &message))?;
                continue;
            }
            Parsed::Request(r) => match r.id {
                Some(id) => id,
                None => continue,
            },
        };
        let Ok(request) = serde_json::from_str::<Value>(text.trim()) else {
            say(&mut output, &protocol::error(Some(&id), INTERNAL_ERROR, "the request couldn't be read again"))?;
            continue;
        };
        let reply = match token.filter(|t| tokens::looks_like_token(t)) {
            Some(token) => relay(&mut connect, token, &request, &id),
            None => protocol::error(Some(&id), TOKEN_REFUSED, TOKEN_TEXT),
        };
        say(&mut output, &reply)?;
    }
}

/// One line and its newline written in one piece, and flushed.
fn say(output: &mut impl Write, line: &str) -> std::io::Result<()> {
    let mut text = String::with_capacity(line.len() + 1);
    text.push_str(line);
    text.push('\n');
    output.write_all(text.as_bytes())?;
    output.flush()
}

/// Send `request` to MailCraft and read its answer: the answer's line as MailCraft wrote it, or an
/// error line of the bridge's own.
fn relay(connect: &mut impl FnMut() -> std::io::Result<(UnixStream, String)>, token: &str, request: &Value, id: &Id) -> String {
    let id = Some(id);
    let (stream, key) = match connect() {
        Ok(connected) => connected,
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported => return protocol::error(id, NOT_RUNNING, NO_SOCKET_TEXT),
        Err(_) => return protocol::error(id, NOT_RUNNING, NOT_RUNNING_TEXT),
    };
    let mut line = json!({"key": key, "mcp": {"token": token, "request": request}}).to_string();
    line.push('\n');
    let sent = stream.set_write_timeout(Some(TIMEOUT)).and_then(|()| {
        let mut writer = &stream;
        writer.write_all(line.as_bytes())
    });
    if sent.is_err() {
        return protocol::error(id, INTERNAL_ERROR, REFUSED_TEXT);
    }
    // The newline of the longest answer is one byte past its limit.
    let mut reply = Vec::new();
    let read = std::io::BufReader::new(Patient::within(&stream, REPLY_WAIT).take(MAX_REPLY + 1)).read_until(b'\n', &mut reply);
    match read {
        Err(e) if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) => protocol::error(id, INTERNAL_ERROR, SLOW_TEXT),
        Err(_) | Ok(0) => protocol::error(id, INTERNAL_ERROR, REFUSED_TEXT),
        Ok(_) if reply.last() == Some(&b'\n') => {
            reply.pop();
            if reply.last() == Some(&b'\r') {
                reply.pop();
            }
            if reply.is_empty() {
                protocol::error(id, INTERNAL_ERROR, "MailCraft sent no answer")
            } else {
                String::from_utf8_lossy(&reply).into_owned()
            }
        }
        Ok(n) if n as u64 > MAX_REPLY => protocol::error(id, INTERNAL_ERROR, "MailCraft's answer is too long to relay (over 8 MiB)"),
        Ok(_) => protocol::error(id, INTERNAL_ERROR, "MailCraft closed the connection before it finished answering"),
    }
}

/// A line read from standard input.
#[derive(Debug, PartialEq, Eq)]
enum Line {
    /// Its bytes, without the line end.
    Text(Vec<u8>),
    /// Longer than the cap: drained to its newline, not kept.
    TooLong,
    /// The input ended (a line cut short by it is dropped).
    Eof,
}

/// The next line of `input`, at most `max` bytes (the line end, `\n` or `\r\n`, not counted); a longer one is read
/// to its newline and thrown away, so the line after it is read normally.
fn read_capped(input: &mut impl BufRead, max: usize) -> std::io::Result<Line> {
    let mut line: Vec<u8> = Vec::new();
    let mut too_long = false;
    loop {
        let chunk = match input.fill_buf() {
            Ok(chunk) => chunk,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if chunk.is_empty() {
            return Ok(Line::Eof);
        }
        let (text, ends) = match chunk.iter().position(|b| *b == b'\n') {
            Some(i) => (chunk.get(..i).unwrap_or_default(), Some(i + 1)),
            None => (chunk, None),
        };
        // One byte over the cap is kept: it may be the `\r` of a line that is exactly `max`.
        if !too_long {
            if line.len().saturating_add(text.len()) > max.saturating_add(1) {
                too_long = true;
                line = Vec::new();
            } else {
                line.extend_from_slice(text);
            }
        }
        let used = ends.unwrap_or(chunk.len());
        input.consume(used);
        if ends.is_some() {
            if !too_long && line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(if too_long || line.len() > max { Line::TooLong } else { Line::Text(line) });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn lines(input: &[u8], max: usize) -> Vec<Line> {
        let mut input = Cursor::new(input.to_vec());
        let mut out = Vec::new();
        loop {
            match read_capped(&mut input, max).unwrap() {
                Line::Eof => return out,
                other => out.push(other),
            }
        }
    }

    fn text(s: &str) -> Line {
        Line::Text(s.as_bytes().to_vec())
    }

    /// A line of exactly the cap is read, one byte more is not and the next line is read
    /// normally; a `\r\n` end isn't counted; a line cut short by the end of input is dropped.
    #[test]
    fn lines_are_read_up_to_the_cap() {
        assert_eq!(lines(b"abcd\nefgh\r\nij", 4), [text("abcd"), text("efgh")]);
        assert_eq!(lines(b"abcde\nfg\n", 4), [Line::TooLong, text("fg")]);
        assert_eq!(lines(b"abcde\r\nfg\n", 4), [Line::TooLong, text("fg")]);
        assert_eq!(lines(b"\n\nx\n", 4), [text(""), text(""), text("x")]);
        assert_eq!(lines(b"", 4), []);
        // Longer than the reader's own buffer, with the next line after it.
        let mut big = vec![b'x'; 100_000];
        big.extend_from_slice(b"\nok\n");
        let mut input = std::io::BufReader::with_capacity(16, Cursor::new(big));
        assert_eq!(read_capped(&mut input, 1000).unwrap(), Line::TooLong);
        assert_eq!(read_capped(&mut input, 1000).unwrap(), text("ok"));
        assert_eq!(read_capped(&mut input, 1000).unwrap(), Line::Eof);
    }

    fn never() -> std::io::Result<(UnixStream, String)> {
        Err(std::io::Error::new(std::io::ErrorKind::NotFound, "nobody listens"))
    }

    fn run(input: &str, token: Option<&str>) -> Vec<Value> {
        let mut out = Vec::new();
        let mut connects = 0;
        bridge(input.as_bytes(), &mut out, token, || {
            connects += 1;
            never()
        })
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.is_empty() || text.ends_with('\n'), "no half-written line: {text:?}");
        text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    fn code(v: &Value) -> i64 {
        v["error"]["code"].as_i64().unwrap_or(0)
    }

    /// What the bridge answers itself: bad lines, the missing token, MailCraft not there; a
    /// notification, an empty line and input that ends mid-line get no output at all.
    #[test]
    fn the_bridge_answers_what_it_can_without_the_app() {
        let token = tokens::make_token(&[1u8; 32]).unwrap();
        let ping = r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#;
        let out = run(&format!("\n{ping}\n[]\nnot json\n{{\"jsonrpc\":\"1.0\",\"id\":3,\"method\":\"x\"}}\n"), Some(&token));
        assert_eq!(
            out.iter().map(code).collect::<Vec<_>>(),
            [NOT_RUNNING as i64, INVALID_REQUEST as i64, PARSE_ERROR as i64, INVALID_REQUEST as i64]
        );
        assert_eq!(out.first().map(|v| v["id"].clone()), Some(json!(7)), "answered with its id");
        assert_eq!(out.get(3).map(|v| v["id"].clone()), Some(json!(3)), "the id it could read");
        // Notifications get nothing, even with no token; a line cut short is dropped.
        assert!(run("{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n", None).is_empty());
        assert!(run(&ping[..20], Some(&token)).is_empty());
        // No token, or one that isn't one: refused before MailCraft is asked.
        for bad in [None, Some("secret"), Some("")] {
            let out = run(&format!("{ping}\n"), bad);
            assert_eq!(out.iter().map(code).collect::<Vec<_>>(), [TOKEN_REFUSED as i64]);
            let message = out[0]["error"]["message"].as_str().unwrap();
            assert!(message.contains("MAILCRAFT_MCP_TOKEN") && !message.contains("secret"), "{message}");
        }
        // An over-long line is refused and the next one read.
        let long = format!("{}\n{ping}\n", "x".repeat(MAX_LINE + 1));
        let out = run(&long, Some(&token));
        assert_eq!(out.iter().map(code).collect::<Vec<_>>(), [INVALID_REQUEST as i64, NOT_RUNNING as i64]);
        assert_eq!(out[0]["id"], Value::Null);
    }

    /// MailCraft not being there says which MailCraft was looked for: the one on this screen (the
    /// display or Windows session) and data folder, which is what picks the socket.
    #[test]
    fn not_running_says_which_mailcraft_was_looked_for() {
        let token = tokens::make_token(&[2u8; 32]).unwrap();
        let out = run("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n", Some(&token));
        let message = out[0]["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("isn't running") && message.contains("screen") && message.contains("data folder") && message.contains("--data"),
            "{message}"
        );
    }
}
