//! `mailcraft-cli mcp`, the bridge local AI apps start: a scripted client on its standard input
//! and output, a stand-in for the app listening on the real single-instance socket (as the app's
//! own socket code answers, one line each way), and, ignored, the real app on a virtual display.
//!
//! Sockets are Unix ones: these tests run where std has them (Windows' come from a crate the
//! CLI doesn't use).
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mailcraft_engine::McpOutcome;
use mailcraft_engine::Session;
use mailcraft_engine::mcp::Permission;
use mailcraft_engine::mcp::tokens::make_token;
use mailcraft_engine::model::dates::Clock;
use mailcraft_net::instance::socket_path;
use serde_json::{Value, json};

/// The key the stand-in app checks, as the real one's `.key` file holds.
const KEY: &str = "a-key-for-the-test";
/// How long a test waits for a line from the bridge.
const WAIT: Duration = Duration::from_secs(30);
const NOW: i64 = 1_700_000_000;

/// A folder of the test's own for a data folder (never one that holds anything of anyone's).
fn data_dir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mailcraft-bridge-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// What the stand-in does with a request: the whole line as JSON and the connection to answer on.
type Handler = Box<dyn FnMut(Value, &mut UnixStream)>;

/// A stand-in for the running MailCraft: listens where the app would for `data`, with its key
/// beside the socket, and answers each connection (one request line) by `handler`.
struct StandIn {
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl StandIn {
    /// `make` builds the handler on the stand-in's thread (a session isn't `Send`).
    fn listen<H: FnMut(Value, &mut UnixStream) + 'static>(data: &Path, make: impl FnOnce() -> H + Send + 'static) -> StandIn {
        let socket = socket_path(data).expect("a private folder for the socket");
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        std::fs::write(socket.with_extension("key"), KEY).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let mut handler: Handler = Box::new(make());
            while !stopped.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                };
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let mut line = String::new();
                if BufReader::new(&stream).read_line(&mut line).is_err() {
                    continue;
                }
                if let Ok(request) = serde_json::from_str::<Value>(line.trim_end()) {
                    handler(request, &mut stream);
                }
            }
        });
        StandIn { socket, stop, thread: Some(thread) }
    }
}

impl Drop for StandIn {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_file(self.socket.with_extension("key"));
    }
}

/// `mailcraft-cli mcp` running, and what it says, line by line.
struct Bridge {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
}

impl Bridge {
    fn start(data: &Path, token: Option<&str>) -> Bridge {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mailcraft-cli"));
        command.arg("mcp").arg("--data").arg(data).env_remove("MAILCRAFT_DATA").env_remove("MAILCRAFT_MCP_TOKEN");
        if let Some(t) = token {
            command.env("MAILCRAFT_MCP_TOKEN", t);
        }
        Bridge::spawn(command)
    }

    fn spawn(mut command: Command) -> Bridge {
        let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let stdin = child.stdin.take();
        let out = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Bridge { child, stdin, lines }
    }

    fn send(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().unwrap();
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    /// The next line it wrote, as JSON.
    fn answer(&mut self) -> Value {
        let line = self.lines.recv_timeout(WAIT).expect("the bridge answers");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"))
    }

    /// A request and its answer.
    fn ask(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string());
        self.answer()
    }

    /// Close its input and wait for it to end; whatever it still wrote is returned.
    fn finish(mut self) -> (std::process::ExitStatus, Vec<String>) {
        drop(self.stdin.take());
        let status = self.child.wait().unwrap();
        (status, self.lines.try_iter().collect())
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn error_of(answer: &Value) -> (i64, String) {
    (answer["error"]["code"].as_i64().unwrap_or(0), answer["error"]["message"].as_str().unwrap_or_default().to_string())
}

/// The app's answer to an MCP request line: what `Session::mcp_handle` says, with the key
/// checked (a request with another is closed on without a reply, as the app does).
fn answering_with(session: &mut Session, request: &Value, calls: &mut i64) -> Option<String> {
    if request["key"] != KEY {
        return None;
    }
    *calls += 1;
    let mcp = &request["mcp"];
    // Two seconds apart: a script is never over the ten calls a second a token may make.
    match session.mcp_handle(mcp["token"].as_str().unwrap_or_default(), &mcp["request"], NOW * 1000 + *calls * 2000) {
        McpOutcome::Reply(line) => Some(line),
        McpOutcome::Later(_) => None,
    }
}

/// The sample mailbox in a session of its own, in a folder of its own, with `token` accepted.
fn sample_session(test: &str, token: &str) -> Session {
    let dir = data_dir(&format!("{test}-sample"));
    let store = mailcraft_engine::store::Store::open(&dir).unwrap();
    assert!(mailcraft_engine::sample::seed_if_empty_at(&store, NOW, 0).unwrap());
    let mut session = Session::open(Some(store), Clock::new(NOW, 0));
    session.mcp_seed_token(token, &[Permission::Read]);
    session
}

/// A client's whole conversation through the bridge: `initialize`, the `initialized`
/// notification (which gets no answer, and isn't relayed), `tools/list`, and `tools/call` of
/// `mail_accounts` and `mail_list`, answered by a real session over the real socket.
#[test]
fn a_scripted_client_works_through_the_bridge() {
    let data = data_dir("script");
    let token = make_token(&[9u8; 32]).unwrap();
    let t = token.clone();
    let _app = StandIn::listen(&data, move || {
        let mut session = sample_session("script", &t);
        let mut calls = 0;
        move |request: Value, stream: &mut UnixStream| {
            if let Some(line) = answering_with(&mut session, &request, &mut calls) {
                let _ = writeln!(stream, "{line}");
            }
        }
    });
    let mut bridge = Bridge::start(&data, Some(&token));
    let init =
        bridge.ask(1, "initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}));
    assert_eq!(
        (init["id"].clone(), init["result"]["protocolVersion"].as_str(), init["result"]["serverInfo"]["name"].as_str()),
        (json!(1), Some("2025-06-18"), Some("mailcraft"))
    );
    bridge.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    let tools = bridge.ask(2, "tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains(&"mail_list") && !names.contains(&"mail_send"), "a read token lists the read tools: {names:?}");
    let accounts = bridge.ask(3, "tools/call", json!({"name": "mail_accounts", "arguments": {}}));
    assert_eq!(accounts["id"], json!(3), "the notification before it got no answer");
    let account = accounts["result"]["structuredContent"]["accounts"][0]["account"].as_str().unwrap().to_string();
    let listed = bridge.ask(4, "tools/call", json!({"name": "mail_list", "arguments": {"account": account, "folder": "INBOX", "limit": 3}}));
    assert_eq!(listed["result"]["isError"], json!(false), "{listed}");
    let messages = listed["result"]["structuredContent"]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert!(messages.iter().all(|m| m["key"].as_str().is_some_and(|k| k.contains("INBOX"))));
    // A token MailCraft doesn't know is refused by MailCraft, through the bridge too.
    let (status, rest) = bridge.finish();
    assert!(status.success() && rest.is_empty(), "{status:?} {rest:?}");
    let other = make_token(&[3u8; 32]).unwrap();
    let mut bridge = Bridge::start(&data, Some(&other));
    let refused = bridge.ask(1, "tools/list", json!({}));
    assert_eq!(error_of(&refused).0, -32001, "{refused}");
}

/// What the bridge relays is the key, the token and the request as it was written: and what the
/// app is told is one request, never the notification.
#[test]
fn the_app_is_sent_the_key_the_token_and_the_request() {
    let data = data_dir("shape");
    let token = make_token(&[4u8; 32]).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&seen);
    let _app = StandIn::listen(&data, move || {
        move |request: Value, stream: &mut UnixStream| {
            let id = request["mcp"]["request"]["id"].clone();
            kept.lock().unwrap().push(request);
            let _ = writeln!(stream, "{}", json!({"jsonrpc": "2.0", "id": id, "result": {}}));
        }
    });
    let mut bridge = Bridge::start(&data, Some(&token));
    bridge.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    bridge.send("");
    let answer = bridge.ask(41, "ping", json!({}));
    assert_eq!(answer, json!({"jsonrpc": "2.0", "id": 41, "result": {}}), "the app's line, as it wrote it");
    // A string id comes back as a string.
    bridge.send(r#"{"jsonrpc":"2.0","id":"abc","method":"ping"}"#);
    assert_eq!(bridge.answer()["id"], json!("abc"));
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "the notification wasn't relayed: {seen:?}");
    assert_eq!(seen[0], json!({"key": KEY, "mcp": {"token": token, "request": {"jsonrpc": "2.0", "id": 41, "method": "ping", "params": {}}}}));
}

/// MailCraft not running (no socket, a socket nobody listens at, or one on another data folder)
/// is said, as the screen and data folder looked for, and the bridge goes on.
#[test]
fn a_mailcraft_that_isnt_running_is_said() {
    let token = make_token(&[5u8; 32]).unwrap();
    let none = data_dir("none");
    let mut bridge = Bridge::start(&none, Some(&token));
    let answer = bridge.ask(1, "tools/list", json!({}));
    let (code, message) = error_of(&answer);
    assert_eq!(code, -32000, "{answer}");
    assert!(message.contains("isn't running") && message.contains("screen") && message.contains("data folder"), "{message}");
    // A socket left by a MailCraft that didn't exit cleanly: nobody listens.
    let stale = data_dir("stale");
    let socket = socket_path(&stale).unwrap();
    drop(UnixListener::bind(&socket).unwrap());
    let mut bridge = Bridge::start(&stale, Some(&token));
    assert_eq!(error_of(&bridge.ask(2, "tools/list", json!({}))).0, -32000);
    let _ = std::fs::remove_file(&socket);
    // Another data folder than the app's is another MailCraft.
    let theirs = data_dir("theirs");
    let _app = StandIn::listen(&theirs, || {
        |_: Value, stream: &mut UnixStream| {
            let _ = writeln!(stream, "{}", json!({"jsonrpc": "2.0", "id": 3, "result": {}}));
        }
    });
    let mut bridge = Bridge::start(&data_dir("mine"), Some(&token));
    assert_eq!(error_of(&bridge.ask(3, "tools/list", json!({}))).0, -32000);
    let mut bridge = Bridge::start(&theirs, Some(&token));
    assert!(bridge.ask(3, "tools/list", json!({}))["result"].is_object(), "the same folder is reached");
}

/// Without a token the bridge answers `-32001` itself, naming the variable and never asking the
/// app; a notification still gets nothing.
#[test]
fn a_bridge_without_a_token_says_so() {
    let data = data_dir("notoken");
    let asked = Arc::new(AtomicBool::new(false));
    let was = Arc::clone(&asked);
    let _app = StandIn::listen(&data, move || move |_: Value, _: &mut UnixStream| was.store(true, Ordering::Release));
    let mut bridge = Bridge::start(&data, None);
    bridge.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    let (code, message) = error_of(&bridge.ask(1, "tools/list", json!({})));
    assert_eq!(code, -32001);
    assert!(message.contains("MAILCRAFT_MCP_TOKEN"), "{message}");
    assert!(!asked.load(Ordering::Acquire), "the app wasn't asked");
}

/// An app that closes without an answer (one that refuses the key, or from before MCP) is said
/// to have refused, and the bridge goes on with the next request; a reply over 8 MiB is refused
/// the same way, without being held.
#[test]
fn an_app_that_doesnt_answer_or_answers_too_much_is_survived() {
    let data = data_dir("refused");
    let token = make_token(&[6u8; 32]).unwrap();
    let step = Arc::new(Mutex::new(0));
    let counted = Arc::clone(&step);
    let _app = StandIn::listen(&data, move || {
        move |request: Value, stream: &mut UnixStream| {
            let n = {
                let mut n = counted.lock().unwrap();
                *n += 1;
                *n
            };
            let id = request["mcp"]["request"]["id"].clone();
            match n {
                // Closed on without a word.
                1 => {}
                // Nine MiB on one line.
                2 => {
                    let mut huge = vec![b'x'; 9 << 20];
                    huge.push(b'\n');
                    let _ = stream.write_all(&huge);
                }
                _ => {
                    let _ = writeln!(stream, "{}", json!({"jsonrpc": "2.0", "id": id, "result": {"ok": true}}));
                }
            }
        }
    });
    let mut bridge = Bridge::start(&data, Some(&token));
    let (code, message) = error_of(&bridge.ask(1, "ping", json!({})));
    assert_eq!(code, -32603);
    assert!(message.contains("refused") && message.contains("--data"), "{message}");
    let (code, message) = error_of(&bridge.ask(2, "ping", json!({})));
    assert_eq!(code, -32603);
    assert!(message.contains("too long"), "{message}");
    let answer = bridge.ask(3, "ping", json!({}));
    assert_eq!(answer["result"]["ok"], json!(true), "still usable: {answer}");
    // Input closed in the middle of a line: it ends, successfully, with nothing half-written.
    let mut bridge = Bridge::start(&data, Some(&token));
    bridge.stdin.as_mut().unwrap().write_all(br#"{"jsonrpc":"2.0","id":1,"meth"#).unwrap();
    let (status, rest) = bridge.finish();
    assert!(status.success(), "{status:?}");
    assert!(rest.iter().all(|l| serde_json::from_str::<Value>(l).is_ok()), "{rest:?}");
}

/// A line of more than a MiB is refused and the next one answered (the app is never asked about
/// the long one).
#[test]
fn a_line_over_a_mebibyte_is_refused_and_the_next_one_read() {
    let data = data_dir("long");
    let token = make_token(&[8u8; 32]).unwrap();
    let _app = StandIn::listen(&data, || {
        |request: Value, stream: &mut UnixStream| {
            let id = request["mcp"]["request"]["id"].clone();
            let _ = writeln!(stream, "{}", json!({"jsonrpc": "2.0", "id": id, "result": {}}));
        }
    });
    let mut bridge = Bridge::start(&data, Some(&token));
    bridge.send(&format!(r#"{{"jsonrpc":"2.0","id":1,"method":"ping","params":{{"x":"{}"}}}}"#, "y".repeat(1 << 20)));
    let refused = bridge.answer();
    assert_eq!((error_of(&refused).0, refused["id"].clone()), (-32600, Value::Null));
    assert!(bridge.ask(2, "ping", json!({}))["result"].is_object());
}

/// Edge case 59: the commands that set MCP up run only from a click in the Connected apps card,
/// so neither `mailcraft-cli run` nor a script can make a token, switch MCP on or set up approval.
#[test]
fn edge_case_59_the_cli_cant_run_the_commands_that_set_mcp_up() {
    let data = data_dir("owner-only");
    let cli = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_mailcraft-cli")).arg("--data").arg(&data).args(args).env_remove("MAILCRAFT_DATA").output().unwrap()
    };
    let made = cli(&["sample"]);
    assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
    for (id, params) in [
        ("mcp.createToken", r#"{"name":"Sneaky","permissions":["read","organize","send"]}"#),
        ("mcp.setEnabled", r#"{"on":true}"#),
        ("mcp.setupApproval", "{}"),
        ("mcp.setRules", r#"{"noCodeReplies":true,"dailyLimit":200}"#),
        ("mcp.revokeToken", r#"{"id":"x"}"#),
    ] {
        let out = cli(&["run", id, params]);
        let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(!out.status.success(), "{id} ran: {stdout}");
        assert!(stderr.contains("Connected apps"), "{id}: {stderr}");
        assert!(!stdout.contains("mcp_"), "no token was made: {stdout}");
    }
    // Nothing was written: no settings, so no token for a bridge to be let in with.
    let kept: Vec<_> = walk(&data).into_iter().filter(|p| p.file_name().is_some_and(|n| n == "mcp.json")).collect();
    assert!(kept.is_empty(), "{kept:?}");
}

/// Every file under `dir`.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                todo.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

/// The same conversation against the real MailCraft (`mailcraft --sample`) on a virtual X
/// display: a token made through the engine, MCP switched on, the app started, and a client's
/// `initialize`, `tools/list` and `tools/call` answered by it. Needs `Xvfb` and the app built
/// (`cargo build -p mailcraft`); run with `cargo test -p mailcraft-cli --test mcp_bridge -- --ignored`.
#[test]
#[ignore = "starts the real app on a virtual X display (Xvfb) and needs `cargo build -p mailcraft` first"]
fn the_real_app_answers_through_the_bridge() {
    let cli = PathBuf::from(env!("CARGO_BIN_EXE_mailcraft-cli"));
    let app = cli.with_file_name("mailcraft");
    assert!(app.is_file(), "build the app first: cargo build -p mailcraft");
    let base = data_dir("real");
    // `--sample --data D` keeps its mailbox in D/sample: made here, with MCP on and a token.
    let root = base.join("sample");
    let token = {
        let store = mailcraft_engine::store::Store::open_alone(&root).unwrap();
        mailcraft_engine::sample::seed_if_empty_at(&store, NOW, 0).unwrap();
        let mut session = Session::open(Some(store.clone()), Clock::new(NOW, 0));
        session.mcp.random = Some(|n| mailcraft_net::oauth::random_bytes(n).ok());
        session.mcp.owner_ui = true;
        session.run("mcp.createToken", &json!({"name": "test", "permissions": ["read"]})).unwrap();
        // Shown once when it's made: only its hash is kept.
        let made = session.mcp.revealed.as_ref().map(|r| r.token.clone()).unwrap();
        session.run("mcp.setEnabled", &json!({"on": true})).unwrap();
        drop(session);
        store.seal_all().unwrap();
        made
    };
    // Its own display, so the app's socket and the bridge's name agree.
    let display = format!(":{}", 90 + std::process::id() % 100);
    let mut xvfb = Command::new("Xvfb").args([&display, "-screen", "0", "1024x768x24"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let screen = Path::new("/tmp/.X11-unix").join(format!("X{}", &display[1..]));
    for _ in 0..100 {
        if screen.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut window = Command::new(&app)
        .args(["--sample", "--data"])
        .arg(&base)
        .env("DISPLAY", &display)
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("MAILCRAFT_DATA")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // The app listens once its window is up: the bridge tries until it answers.
    let mut command = Command::new(&cli);
    command.arg("mcp").arg("--data").arg(&root).env("DISPLAY", &display).env_remove("WAYLAND_DISPLAY").env("MAILCRAFT_MCP_TOKEN", &token);
    let mut bridge = Bridge::spawn(command);
    let mut init = Value::Null;
    for i in 0..60 {
        init = bridge.ask(i, "initialize", json!({"protocolVersion": "2025-06-18"}));
        if init["result"].is_object() {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    let tools = bridge.ask(100, "tools/list", json!({}));
    let account = bridge.ask(101, "tools/call", json!({"name": "mail_accounts", "arguments": {}}));
    let acct = account["result"]["structuredContent"]["accounts"][0]["account"].as_str().map(str::to_string);
    let listed = acct
        .as_ref()
        .map(|a| bridge.ask(102, "tools/call", json!({"name": "mail_list", "arguments": {"account": a, "folder": "INBOX", "limit": 2}})));
    let _ = window.kill();
    let _ = window.wait();
    let _ = xvfb.kill();
    let _ = xvfb.wait();
    assert_eq!(init["result"]["serverInfo"]["name"], json!("mailcraft"), "{init}");
    assert!(tools["result"]["tools"].as_array().is_some_and(|t| !t.is_empty()), "{tools}");
    assert_eq!(listed.map(|l| l["result"]["structuredContent"]["messages"].as_array().map(Vec::len)), Some(Some(2)));
    let _ = std::fs::remove_dir_all(&base);
}
