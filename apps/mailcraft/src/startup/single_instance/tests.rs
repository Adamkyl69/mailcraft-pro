use super::*;
use mailcraft_net::instance::READ_SLICE;

/// What a request hands over, `None` when it isn't a launch's request with the key.
fn hand(line: &str, key: &str) -> Option<Handed> {
    match parse_request(line, key) {
        Some(Request::Hand(handed)) => Some(handed),
        _ => None,
    }
}

/// Where a test's socket goes, in a folder of its own, its name kept short: a socket's whole
/// path must fit its address (103 bytes on macOS, whose temporary folder alone takes about 50).
fn socket_at(test: &str) -> PathBuf {
    std::env::temp_dir().join(format!("mci-{:x}-{test}", std::process::id())).join(mailcraft_net::instance::socket_name(Path::new("C:/data"), ""))
}

/// That socket's folder, emptied.
fn socket(test: &str) -> PathBuf {
    let path = socket_at(test);
    if let Some(dir) = path.parent() {
        let _ = std::fs::remove_dir_all(dir);
    }
    path
}

/// Every socket fits a Unix socket's address (`sun_path`: 104 bytes on macOS, 108 elsewhere,
/// the NUL included). One that doesn't can't be listened at, and every launch opened a window
/// of its own: the tests' did on the macOS CI runner, with a five-digit process id.
#[test]
fn socket_paths_fit_a_socket_address() {
    let app = mailcraft_net::instance::socket_path(Path::new("/home/zoe/.local/share/mailcraft"));
    assert_eq!(app.is_some(), mailcraft_engine::store::scratch_dir("instance").is_some(), "a socket wherever its folder can be");
    let app = app.unwrap_or_default();
    for p in [
        app,
        socket_at("together"),
        socket_at("stale"),
        socket_at("silent"),
        socket_at("show"),
        socket_at("keyed"),
        socket_at("handed"),
        socket_at("lost"),
        socket_at("early"),
        socket_at("patient"),
    ] {
        let len = p.as_os_str().len();
        assert!(len < 104, "{len} bytes: {}", p.display());
    }
}

/// The first launch listens; a second asks it to show its window and exits, and the first
/// sees the ask once; closing removes the socket, and the next launch is the first again.
#[test]
fn a_second_launch_shows_the_first() {
    let path = socket("show");
    let Claim::Primary(first) = claim_at(&path, &Handed::default()) else { panic!("the first launch listens") };
    assert!(first.take().is_none());
    assert!(matches!(claim_at(&path, &Handed::default()), Claim::Shown));
    assert!(matches!(claim_at(&path, &Handed::default()), Claim::Shown));
    assert_eq!(first.take(), Some(Handed::default()), "asked");
    assert!(first.take().is_none(), "once");
    // Woken when the window exists.
    let ctx = egui::Context::default();
    first.connect(&ctx);
    assert!(matches!(claim_at(&path, &Handed::default()), Claim::Shown));
    assert!(first.take().is_some());
    first.close();
    assert!(std::fs::symlink_metadata(&path).is_err(), "the socket goes");
    assert!(matches!(claim_at(&path, &Handed::default()), Claim::Primary(_)), "no one listens: this one is first");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// Found in audit (#242): a launch whose connect failed just before another bound the
/// socket (without the election lock, which is best effort) wrote its key over the other's,
/// then found the socket taken. The running MailCraft's later launches sent that key and
/// were refused, each opening a window of its own. A key takes its name only once its
/// launch listens.
#[test]
fn a_launch_that_loses_the_socket_leaves_the_key() {
    let path = socket("lost");
    let Claim::Primary(first) = claim_at(&path, &Handed::default()) else { panic!("the first launch listens") };
    let key = read_key(&path).unwrap();
    // The other launch's step after its connect failed, while the first listens.
    assert!(matches!(listen(&path), Ok(None)), "the socket is the first's");
    assert_eq!(read_key(&path).unwrap(), key, "the first's key stays");
    assert!(matches!(claim_at(&path, &Handed::default()), Claim::Shown), "later launches still reach it");
    assert_eq!(first.take(), Some(Handed::default()));
    // Nothing of the other launch's is left beside the socket: the socket, its lock, its key.
    let dir = path.parent().unwrap();
    let mut left: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    let name = |ext: &str| path.with_extension(ext).file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(left, [name("key"), name("lock"), name("sock")]);
    first.close();
    let _ = std::fs::remove_dir_all(dir);
}

/// A launch that reaches the running MailCraft before its key is in place (it listens first;
/// the election lock, when it can't be had, doesn't keep the launch out) asks again once it
/// is: with no key beside the socket, or the key of the run before, it was refused and opened
/// a window of its own.
#[test]
fn a_launch_before_the_key_is_in_place_asks_again() {
    let path = socket("early");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let open = Handed::opening(vec!["mailto:ana@x.example".to_string()]);
    for before in [None, Some("the-key-of-the-run-before")] {
        // Listening, its key not yet under its name.
        let listener = UnixListener::bind(&path).unwrap();
        let Claim::Primary(first) = serve(listener, &path, "this-runs-key".into()) else { panic!("it answers") };
        let _ = std::fs::remove_file(path.with_extension("key"));
        if let Some(old) = before {
            write_key(&path.with_extension("key"), old).unwrap();
        }
        let mut asked = 0;
        let stream = UnixStream::connect(&path).unwrap();
        let handed = hand_over(stream, &path, &open, || {
            asked += 1;
            // The key takes its name after the launch's second ask.
            if asked == 2 {
                write_key(&path.with_extension("key"), "this-runs-key").unwrap();
            }
        });
        assert!(handed.is_ok(), "{before:?}: {handed:?}");
        assert_eq!((asked, first.take()), (2, Some(open.clone())), "{before:?}");
        // A key that never comes: given up on after a few asks, for a window of its own.
        let _ = std::fs::remove_file(path.with_extension("key"));
        let mut asked = 0;
        let stream = UnixStream::connect(&path).unwrap();
        assert!(hand_over(stream, &path, &open, || asked += 1).is_err());
        assert_eq!((asked, first.take()), (KEY_TRIES - 1, None));
        first.close();
    }
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// Found while a test failed one run in twenty: a read on the socket is made again every
/// [`READ_SLICE`] until its time is up, so a close Windows didn't wake it for is seen at the
/// next (it waited out all of [`TIMEOUT`], and the ask behind it timed out). What comes after
/// a few slices is still read, and the read gives up when its time is.
#[test]
fn a_read_is_made_again_until_its_time_is_up() {
    use std::io::{Read, Write};
    let path = socket("patient");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(&path).unwrap();
    let mut late = UnixStream::connect(&path).unwrap();
    let (ours, _) = listener.accept().unwrap();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(READ_SLICE * 4);
        late.write_all(b"ok\n").unwrap();
        late
    });
    let mut got = [0u8; 3];
    Patient::new(&ours).read_exact(&mut got).unwrap();
    assert_eq!(&got, b"ok\n", "read after four slices");
    let late = writer.join().unwrap();
    // Nothing more comes: the read gives up at its time, not at the first slice.
    let started = std::time::Instant::now();
    let until = started + READ_SLICE * 5;
    let gave_up = Patient { stream: &ours, until }.read(&mut got);
    assert_eq!(gave_up.map_err(|e| e.kind()), Err(std::io::ErrorKind::TimedOut));
    assert!(started.elapsed() >= READ_SLICE * 5, "{:?}", started.elapsed());
    // A close is an end, as before.
    drop(late);
    assert_eq!(Patient::new(&ours).read(&mut got).unwrap(), 0);
    drop((ours, listener));
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// Several launches at once (a double click and then some): one window.
#[test]
fn launches_started_together_open_one_window() {
    let path = socket("together");
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let launches: Vec<_> = (0..8)
        .map(|_| {
            let (path, barrier) = (path.clone(), Arc::clone(&barrier));
            std::thread::spawn(move || {
                barrier.wait();
                claim_at(&path, &Handed::default())
            })
        })
        .collect();
    let claims: Vec<Claim> = launches.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(claims.iter().filter(|c| matches!(c, Claim::Primary(_))).count(), 1, "one window");
    assert_eq!(claims.iter().filter(|c| matches!(c, Claim::Shown)).count(), 7, "the rest show it");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// A socket left by a MailCraft that didn't exit cleanly is replaced; something that isn't
/// MailCraft at the other end, or junk sent to it, opens a window of its own.
#[test]
fn a_stale_socket_is_replaced_and_junk_is_refused() {
    let path = socket("stale");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    drop(UnixListener::bind(&path).unwrap());
    assert!(std::fs::symlink_metadata(&path).is_ok(), "left behind");
    let Claim::Primary(first) = claim_at(&path, &Handed::default()) else { panic!("replaced") };
    {
        use std::io::{Read, Write};
        let mut s = UnixStream::connect(&path).unwrap();
        // Bounded, as a launch's own read is: a close Windows loses can't hang the test.
        s.set_read_timeout(Some(TIMEOUT * 2)).unwrap();
        s.write_all(b"open C:/x\n").unwrap();
        let mut reply = Vec::new();
        let _ = s.read_to_end(&mut reply);
        assert!(reply.is_empty(), "no ok for junk");
    }
    assert!(first.take().is_none());
    first.close();

    // A listener that never answers: a window of its own after the wait.
    let silent = socket("silent");
    std::fs::create_dir_all(silent.parent().unwrap()).unwrap();
    let _listener = UnixListener::bind(&silent).unwrap();
    assert!(matches!(claim_at(&silent, &Handed::default()), Claim::Alone));
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    let _ = std::fs::remove_dir_all(silent.parent().unwrap());
}

/// What a launch hands over reaches the running window, in order, once; the request carries
/// the run's key, which is in a file beside the socket and new each run.
#[test]
fn a_launch_hands_over_its_links_and_files() {
    let path = socket("handed");
    let Claim::Primary(first) = claim_at(&path, &Handed::default()) else { panic!("the first launch listens") };
    let key = read_key(&path).unwrap();
    assert_eq!(key, first.key);
    assert!(key.len() >= 40, "{key}");
    let open = vec!["mailto:ana@x.example?subject=Hi".to_string(), "C:/Users/zoe/Downloads/Invite.ics".to_string()];
    assert!(matches!(claim_at(&path, &Handed::opening(open.clone())), Claim::Shown));
    assert!(matches!(claim_at(&path, &Handed::opening(vec!["/home/zoe/x.eml".to_string()])), Claim::Shown));
    let mut all = open.clone();
    all.push("/home/zoe/x.eml".into());
    assert_eq!(first.take(), Some(Handed::opening(all)));
    assert!(first.take().is_none());
    first.close();
    assert!(std::fs::symlink_metadata(path.with_extension("key")).is_err(), "the key goes with the socket");
    let Claim::Primary(second) = claim_at(&path, &Handed::default()) else { panic!("first again") };
    assert_ne!(second.key, key, "a new key each run");
    second.close();
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// Another program on the computer can't open anything through the socket: a request without
/// the key, with another key, or in the old form gets no `ok` and hands nothing over.
#[test]
fn requests_without_the_key_are_refused() {
    use std::io::{Read, Write};
    let path = socket("keyed");
    let Claim::Primary(first) = claim_at(&path, &Handed::default()) else { panic!("the first launch listens") };
    let guesses = [
        "show\n".to_string(),
        "{\"open\":[\"mailto:a@x.example\"]}\n".into(),
        request("", &Handed::opening(vec!["mailto:a@x.example".into()])),
        request("not-the-key", &Handed::opening(vec!["mailto:a@x.example".into()])),
        request(&format!("{}x", first.key), &Handed::opening(vec!["mailto:a@x.example".into()])),
        request(first.key.get(1..).unwrap(), &Handed::opening(vec!["mailto:a@x.example".into()])),
        format!("{{\"key\":\"{}\",\"open\":\"mailto:a@x.example\"}}\n", first.key),
        format!("{{\"key\":\"{}\",\"open\":[1]}}\n", first.key),
    ];
    for g in &guesses {
        let mut s = UnixStream::connect(&path).unwrap();
        s.set_read_timeout(Some(TIMEOUT * 2)).unwrap();
        s.write_all(g.as_bytes()).unwrap();
        let mut reply = Vec::new();
        let _ = s.read_to_end(&mut reply);
        assert!(reply.is_empty(), "no ok for {g:?}");
    }
    assert!(first.take().is_none(), "nothing handed over");
    // The right key: accepted.
    let mut s = UnixStream::connect(&path).unwrap();
    s.set_read_timeout(Some(TIMEOUT * 2)).unwrap();
    s.write_all(request(&first.key, &Handed::opening(vec!["mailto:a@x.example".into()])).as_bytes()).unwrap();
    let mut reply = String::new();
    let _ = s.read_to_string(&mut reply);
    assert_eq!(reply, OK);
    assert_eq!(first.take(), Some(Handed::opening(vec!["mailto:a@x.example".to_string()])));
    first.close();
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// The request's parser: the key exactly, a list of strings, empties and extras left out.
#[test]
fn requests_are_read_strictly() {
    let open: Vec<String> = (0..100).map(|i| format!("/m/{i}.eml")).collect();
    let line = request("k3y", &Handed::opening(open.clone()));
    assert!(line.ends_with('\n') && !line.trim_end().contains('\n'), "one line");
    assert_eq!(hand(&line, "k3y").map(|h| h.open.len()), Some(MAX_OPEN));
    assert_eq!(hand(&line, "k3Y"), None);
    assert_eq!(hand(&line, ""), None, "no key, no requests");
    assert_eq!(hand(&request("", &Handed::default()), ""), None);
    assert_eq!(hand("{\"key\":\"k\",\"open\":[\"\",\" \",\"a\"]}", "k"), Some(Handed::opening(vec!["a".to_string()])));
    for junk in ["", "{}", "[]", "null", "{\"key\":\"k\"}", "{\"key\":5,\"open\":[]}", "\u{0}\u{ff}"] {
        assert_eq!(hand(junk, "k"), None, "{junk:?}");
    }
    assert!(keys_match(b"abc", b"abc") && !keys_match(b"abc", b"abd") && !keys_match(b"abc", b"ab"));
}

/// Issue #96: a launch's files to send reach the running window as one group, a new message
/// each; a request from a MailCraft before 0.39 (no `attach`) is read as ever, and one whose
/// `attach` isn't a list of strings is refused.
#[test]
fn a_launch_hands_over_its_files_to_send() {
    let path = socket("attach");
    let Claim::Primary(first) = claim_at(&path, &Handed::default()) else { panic!("the first launch listens") };
    let files = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let one = Handed { open: Vec::new(), attach: vec![files(&["/u/report.pdf", "/u/photo.jpg"])] };
    let two = Handed { open: files(&["mailto:a@x.example"]), attach: vec![files(&["/u/notes.txt"])] };
    assert!(matches!(claim_at(&path, &one), Claim::Shown));
    assert!(matches!(claim_at(&path, &two), Claim::Shown));
    let got = first.take().unwrap();
    assert_eq!(got.open, files(&["mailto:a@x.example"]));
    assert_eq!(got.attach, [files(&["/u/report.pdf", "/u/photo.jpg"]), files(&["/u/notes.txt"])], "a message each, in order");
    // As many as wait at once.
    for _ in 0..MAX_WAITING_SENDS + 4 {
        first.ask(one.clone());
    }
    assert_eq!(first.take().unwrap().attach.len(), MAX_WAITING_SENDS);
    first.close();
    let _ = std::fs::remove_dir_all(path.parent().unwrap());

    let many: Vec<String> = (0..500).map(|i| format!("/u/{i}.pdf")).collect();
    let read = hand(&request("k", &Handed { open: Vec::new(), attach: vec![many] }), "k").unwrap();
    assert_eq!(read.attach.iter().map(Vec::len).collect::<Vec<_>>(), [MAX_ATTACHED]);
    assert_eq!(hand("{\"key\":\"k\",\"open\":[\"a\"]}", "k"), Some(Handed::opening(vec!["a".to_string()])), "before 0.39");
    assert_eq!(hand("{\"key\":\"k\",\"open\":[],\"attach\":[\"\",\" \"]}", "k"), Some(Handed::default()), "nothing to send");
    for junk in [
        "{\"key\":\"k\",\"open\":[],\"attach\":\"/u/a.pdf\"}",
        "{\"key\":\"k\",\"open\":[],\"attach\":[1]}",
        "{\"key\":\"k\",\"attach\":[\"/u/a.pdf\"]}",
    ] {
        assert_eq!(hand(junk, "k"), None, "{junk:?}");
    }
}

// ---- MCP calls (`mcp.rs`) ----

mod mcp_calls {
    use std::io::{BufRead, Write};
    use std::sync::mpsc::sync_channel;

    use mailcraft_engine::mcp::protocol::{INTERNAL_ERROR, SLOW_DOWN, TOKEN_REFUSED};
    use mailcraft_engine::mcp::tokens::make_token;
    use mailcraft_engine::mcp::{MAX_LINE, Permission};
    use mailcraft_engine::model::dates::Clock;
    use mailcraft_engine::model::{Event, Folder, FolderRole, SyncStatus};
    use mailcraft_engine::{McpOutcome, Session};
    use serde_json::{Value, json};

    use super::super::mcp::{Asked, Desk, MAX_IN_FLIGHT, serve_one};
    use super::*;

    const NOW: i64 = 1_700_000_000;

    fn token() -> String {
        make_token(&[7u8; 32]).unwrap()
    }

    /// The sample mailbox in a folder of the test's own, with [`token`] accepted for reading.
    fn session(test: &str) -> Session {
        let dir = std::env::temp_dir().join(format!("mci-{:x}-{test}-data", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = mailcraft_engine::store::Store::open(&dir).unwrap();
        mailcraft_engine::sample::seed_if_empty_at(&store, NOW, 0).unwrap();
        let mut s = Session::open(Some(store), Clock::new(NOW, 0));
        s.mcp_seed_token(&token(), &[Permission::Read, Permission::Organize]);
        s
    }

    fn rpc(id: i64, method: &str, params: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    }

    fn mcp_line(key: &str, token: &str, request: &Value) -> String {
        format!("{}\n", json!({"key": key, "mcp": {"token": token, "request": request}}))
    }

    /// One request line sent on a new connection, and the line that comes back (`None`: the
    /// connection closed without one).
    fn exchange(path: &Path, line: &str) -> Option<String> {
        let mut stream = UnixStream::connect(path).unwrap();
        stream.write_all(line.as_bytes()).unwrap();
        let mut reply = String::new();
        let read = std::io::BufReader::new(Patient::within(&stream, Duration::from_secs(20))).read_line(&mut reply);
        (read.unwrap() > 0).then_some(reply)
    }

    /// [`exchange`] on a thread while this one plays the window: `serve` is run until the answer is in.
    fn answered(path: &Path, line: String, mut serve: impl FnMut()) -> Option<String> {
        let path = path.to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(exchange(&path, &line)).unwrap());
        let until = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            serve();
            if let Ok(got) = rx.try_recv() {
                return got;
            }
            assert!(std::time::Instant::now() < until, "no answer");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn json_of(line: Option<String>) -> Value {
        serde_json::from_str(line.expect("an answer").trim_end()).unwrap()
    }

    /// An MCP request is read as one only with the key, a token string and a request object, and
    /// never together with a launch's `open`.
    #[test]
    fn only_a_request_with_the_key_and_a_request_object_is_an_mcp_call() {
        let ping = rpc(1, "ping", json!({}));
        let good = mcp_line("k", "mcp_x", &ping);
        assert_eq!(parse_request(&good, "k"), Some(Request::Mcp { token: "mcp_x".into(), request: ping.clone() }));
        for (what, line) in [
            ("no key", json!({"mcp": {"token": "t", "request": ping}}).to_string()),
            ("the wrong key", mcp_line("other", "t", &ping)),
            ("open and mcp", json!({"key": "k", "open": [], "mcp": {"token": "t", "request": ping}}).to_string()),
            ("no token", json!({"key": "k", "mcp": {"request": ping}}).to_string()),
            ("a token that isn't text", json!({"key": "k", "mcp": {"token": 5, "request": ping}}).to_string()),
            ("a long token", mcp_line("k", &"t".repeat(129), &ping)),
            ("no request", json!({"key": "k", "mcp": {"token": "t"}}).to_string()),
            ("a batch", mcp_line("k", "t", &json!([ping]))),
            ("a request that isn't an object", mcp_line("k", "t", &json!("ping"))),
            ("an mcp that isn't an object", json!({"key": "k", "mcp": "x"}).to_string()),
        ] {
            assert_eq!(parse_request(&line, "k"), None, "{what}");
        }
        assert_eq!(parse_request(&good, ""), None, "no key, no requests");
    }

    /// The window answers an AI app's call through the real socket: a notification with an empty
    /// line, a bad token with `-32001`, a request the size the bridge reads at most, and a read of the
    /// sample mailbox; the key is the launches' own.
    #[test]
    fn a_call_is_answered_by_the_window_over_the_socket() {
        let path = socket("mcp");
        let Claim::Primary(first) = claim_at(&path, &Handed::default()) else { panic!("the first launch listens") };
        let key = read_key(&path).unwrap();
        let (mut s, mut desk, ctx) = (session("mcp"), Desk::default(), egui::Context::default());
        let mut over = |request: &Value, token: &str| answered(&path, mcp_line(&key, token, request), || desk.serve(Some(&first), &mut s, &ctx));
        let init = json_of(over(&rpc(1, "initialize", json!({"protocolVersion": "2025-06-18"})), &token()));
        assert_eq!((init["id"].clone(), init["result"]["serverInfo"]["name"].as_str()), (json!(1), Some("mailcraft")));
        let note = over(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}), &token());
        assert_eq!(note.as_deref(), Some("\n"), "a notification's answer is an empty line");
        let refused = json_of(over(&rpc(2, "tools/list", json!({})), &make_token(&[1u8; 32]).unwrap()));
        assert_eq!((refused["id"].clone(), refused["error"]["code"].clone()), (json!(2), json!(TOKEN_REFUSED)));
        let accounts = json_of(over(&rpc(3, "tools/call", json!({"name": "mail_accounts", "arguments": {}})), &token()));
        assert_eq!(accounts["result"]["structuredContent"]["accounts"][0]["account"], json!("sample"), "{accounts}");
        let listed = json_of(over(
            &rpc(4, "tools/call", json!({"name": "mail_list", "arguments": {"account": "sample", "folder": "INBOX", "limit": 2}})),
            &token(),
        ));
        assert_eq!(listed["result"]["structuredContent"]["messages"].as_array().map(Vec::len), Some(2), "{listed}");
        // A request of the size the bridge reads at most (the wrapper's own bytes are allowed for).
        let pad = "x".repeat(MAX_LINE - 100);
        let big = json_of(over(&rpc(5, "ping", json!({"pad": pad})), &token()));
        assert_eq!(big["result"], json!({}));
        first.close();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A request without the key, with `open` and `mcp` together, with neither, or that isn't a
    /// request is closed on with no reply (as a launch's always was); the next call is answered.
    #[test]
    fn a_call_without_the_key_is_closed_on() {
        let path = socket("mcpkey");
        let Claim::Primary(first) = claim_at(&path, &Handed::default()) else { panic!("the first launch listens") };
        let key = read_key(&path).unwrap();
        let ping = rpc(1, "ping", json!({}));
        let (mut s, mut desk, ctx) = (session("mcpkey"), Desk::default(), egui::Context::default());
        for line in [
            mcp_line("not-the-key", &token(), &ping),
            json!({"mcp": {"token": token(), "request": ping}}).to_string() + "\n",
            json!({"key": key, "open": [], "mcp": {"token": token(), "request": ping}}).to_string() + "\n",
            json!({"key": key}).to_string() + "\n",
            json!({"key": key, "mcp": {"token": token(), "request": [ping]}}).to_string() + "\n",
            "not json\n".to_string(),
        ] {
            assert_eq!(answered(&path, line.clone(), || desk.serve(Some(&first), &mut s, &ctx)), None, "{line}");
        }
        assert_eq!(s.mcp.settings.tokens[0].last_used, None, "none of them reached the session");
        let ok = json_of(answered(&path, mcp_line(&key, &token(), &ping), || desk.serve(Some(&first), &mut s, &ctx)));
        assert_eq!(ok["result"], json!({}));
        first.close();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Four calls waiting for the window are the most answered at once: the fifth is told
    /// MailCraft is busy (`-32002`), a launch is still shown at once (no thread of the socket is
    /// kept by a call), and the four, let go, say MailCraft is closing.
    #[test]
    fn a_fifth_call_is_told_to_slow_down_and_a_launch_isnt_kept_waiting() {
        let path = socket("mcpfive");
        let Claim::Primary(first) = claim_at(&path, &Handed::default()) else { panic!("the first launch listens") };
        let key = read_key(&path).unwrap();
        let line = mcp_line(&key, &token(), &rpc(9, "ping", json!({})));
        let mut waiting: Vec<UnixStream> = (0..MAX_IN_FLIGHT)
            .map(|_| {
                let mut c = UnixStream::connect(&path).unwrap();
                c.write_all(line.as_bytes()).unwrap();
                c
            })
            .collect();
        let until = std::time::Instant::now() + Duration::from_secs(10);
        while first.asks.mcp.lock().unwrap().len() < MAX_IN_FLIGHT {
            assert!(std::time::Instant::now() < until, "the four calls are queued");
            std::thread::sleep(Duration::from_millis(5));
        }
        let busy = json_of(exchange(&path, &line));
        assert_eq!((busy["id"].clone(), busy["error"]["code"].clone()), (json!(9), json!(SLOW_DOWN)), "{busy}");
        // A launch during the calls: shown within the time a launch waits.
        let started = std::time::Instant::now();
        assert!(matches!(claim_at(&path, &Handed::default()), Claim::Shown));
        assert!(started.elapsed() < TIMEOUT, "{:?}", started.elapsed());
        // The window never takes them: dropped, the four threads answer and end.
        drop(first.take_mcp());
        for c in &mut waiting {
            let mut reply = String::new();
            std::io::BufReader::new(Patient::within(c, Duration::from_secs(10))).read_line(&mut reply).unwrap();
            let v: Value = serde_json::from_str(reply.trim_end()).unwrap();
            assert_eq!(v["error"]["code"], json!(INTERNAL_ERROR), "{reply}");
            assert!(v["error"]["message"].as_str().unwrap().contains("closing"), "{reply}");
        }
        let until = std::time::Instant::now() + Duration::from_secs(10);
        while first.asks.in_flight.load(Ordering::Acquire) != 0 {
            assert!(std::time::Instant::now() < until, "the count goes down as the threads end");
            std::thread::sleep(Duration::from_millis(5));
        }
        first.close();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A server of its own for the window-side tests: no socket, only the queue.
    fn server() -> Server {
        Server { asks: Arc::default(), path: PathBuf::new(), key: "k".into() }
    }

    fn queue(server: &Server, request: Value, wait: Duration) -> std::sync::mpsc::Receiver<String> {
        let (reply, rx) = sync_channel(1);
        server.asks.mcp.lock().unwrap().push_back(Asked { token: token(), request, reply, deadline: std::time::Instant::now() + wait });
        rx
    }

    /// The window never takes the call (or takes it too late): the connection's thread answers
    /// "didn't answer in time" when its wait is over, and the window that finds the call past its
    /// deadline drops it without running it. A call whose connection has gone is still run, and
    /// its answer is let go without a word or a panic.
    #[test]
    fn a_call_the_window_is_late_for_is_dropped_and_one_nobody_waits_for_is_passed_over() {
        let path = socket("mcpwait");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(&path).unwrap();
        let asks: Arc<Asks> = Arc::default();
        let thread_asks = Arc::clone(&asks);
        let client = UnixStream::connect(&path).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let wait = Duration::from_millis(200);
        let serving = std::thread::spawn(move || serve_one(stream, &thread_asks, token(), rpc(5, "ping", json!({})), wait));
        let mut reply = String::new();
        std::io::BufReader::new(Patient::within(&client, Duration::from_secs(10))).read_line(&mut reply).unwrap();
        serving.join().unwrap();
        let v: Value = serde_json::from_str(reply.trim_end()).unwrap();
        assert_eq!((v["id"].clone(), v["error"]["code"].clone()), (json!(5), json!(INTERNAL_ERROR)), "{reply}");
        assert!(v["error"]["message"].as_str().unwrap().contains("in time"), "{reply}");
        // The window finds the call (past its deadline) and doesn't run it.
        let server = Server { asks: Arc::clone(&asks), path: PathBuf::new(), key: "k".into() };
        let (mut s, mut desk, ctx) = (session("mcpwait"), Desk::default(), egui::Context::default());
        desk.serve(Some(&server), &mut s, &ctx);
        assert!(server.asks.mcp.lock().unwrap().is_empty(), "taken");
        assert_eq!(s.mcp.settings.tokens[0].last_used, None, "not run");
        // The receiver gone before the answer: run, and the answer dropped.
        let gone = queue(&server, rpc(6, "ping", json!({})), Duration::from_secs(60));
        drop(gone);
        desk.serve(Some(&server), &mut s, &ctx);
        assert!(s.mcp.settings.tokens[0].last_used.is_some(), "run");
        drop((client, listener));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A call that waits for the network (`mail_sync` on an account with a server) is kept by
    /// the window until the session has its answer, and one the connection gave up on is let go
    /// of, so the token can call again.
    #[test]
    fn a_call_that_waits_for_the_network_is_answered_later_or_let_go() {
        let dir = std::env::temp_dir().join(format!("mci-{:x}-mcplater-data", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = Session::open(Some(mailcraft_engine::store::Store::open(&dir).unwrap()), Clock::new(NOW, 0));
        let id = s.run("account.add", &json!({"email": "zoe@example.org", "password": "pw"})).unwrap()["account"].as_str().unwrap().to_string();
        let folder = Folder { account: id.clone(), path: "INBOX".into(), role: FolderRole::Inbox, selectable: true, ..Default::default() };
        s.apply(Event::Folders { account: id.clone(), folders: vec![folder] });
        s.take_jobs();
        s.mcp_seed_token(&token(), &[Permission::Read, Permission::Organize]);
        let (server, mut desk, ctx) = (server(), Desk::default(), egui::Context::default());
        let sync = rpc(1, "tools/call", json!({"name": "mail_sync", "arguments": {}}));
        let rx = queue(&server, sync.clone(), Duration::from_secs(60));
        desk.serve(Some(&server), &mut s, &ctx);
        assert!(rx.try_recv().is_err(), "not answered yet");
        assert!(s.mcp_waiting());
        // Taken as a frame goes by, finished when the account says so.
        desk.serve(Some(&server), &mut s, &ctx);
        assert!(rx.try_recv().is_err());
        s.apply(Event::Status { account: id.clone(), status: SyncStatus::Online { at: NOW } });
        desk.serve(Some(&server), &mut s, &ctx);
        let done: Value = serde_json::from_str(&rx.try_recv().expect("answered")).unwrap();
        assert_eq!(done["result"]["structuredContent"]["done"], json!([id]), "{done}");
        assert!(!s.mcp_waiting());
        // A connection that gave up: its deadline passes, the window lets go of the call, and the
        // token (one call at a time) isn't kept busy.
        let rx = queue(&server, sync.clone(), Duration::from_millis(300));
        desk.serve(Some(&server), &mut s, &ctx);
        assert!(s.mcp_waiting());
        std::thread::sleep(Duration::from_millis(400));
        desk.serve(Some(&server), &mut s, &ctx);
        assert!(!s.mcp_waiting(), "let go");
        assert!(rx.try_recv().is_err(), "nothing was sent to a connection that had gone");
        let again = s.mcp_handle(&token(), &sync, 5_000_000_000_000);
        assert!(matches!(again, McpOutcome::Later(_)), "not told to slow down: {again:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
