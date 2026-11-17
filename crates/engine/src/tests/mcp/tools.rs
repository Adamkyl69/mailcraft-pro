//! The tools over the sample mailbox: what they answer, and what they leave as it was.

use mailcraft_model::Job;

use super::*;
use crate::mcp::{LogLine, LogQuery};

/// Run `ask` as a `tools/call`, whatever it comes to: an answer, or waiting (forgotten).
fn survives(s: &mut Session, t: &str, name: &str, args: &Value) {
    match s.mcp_handle(t, &request(1, "tools/call", json!({"name": name, "arguments": args})), tick()) {
        McpOutcome::Reply(line) => assert!(serde_json::from_str::<Value>(&line).is_ok(), "{name}: {line}"),
        McpOutcome::Later(ticket) => s.mcp_forget(ticket),
    }
}

#[test]
fn every_mcp_tool_survives_junk_arguments() {
    let (mut s, t) = mcp_session("mcp-junk", &ALL);
    let tools: Vec<&str> = crate::mcp::tools::TOOLS.iter().map(|x| x.name).collect();
    let key = inbox(&mut s)[0].to_id();
    let junk = [
        json!(null),
        json!({}),
        json!({"key": "x"}),
        json!({"key": 5, "folder": [], "account": {"a": 1}}),
        json!({"value": "sideways", "index": -1, "path": "", "data": [999, -4]}),
        json!({"query": "\"unterminated from: -", "scope": 3}),
        json!({"email": "@", "password": "", "imapPort": 99_999_999}),
        json!({"index": 1e300, "limit": -5, "on": "yes"}),
        json!({"limit": 1e300}),
        json!({"keys": "x"}),
        json!({"keys": [1, 2]}),
        json!({"query": ""}),
        json!({"to": []}),
        json!({"body": "b".repeat(600 << 10)}),
        json!({"command": "mail.markRead", "keys": [key.clone()], "params": [1]}),
        json!({"command": "mail.move", "keys": [key.clone()], "params": {"folder": 5}}),
        json!({"account": "sample", "folder": "INBOX", "limit": 3, "offset": 1e300}),
        json!({"key": key.clone(), "headers": "yes"}),
        json!({"since": "yesterday", "errors_only": 3}),
    ];
    let caught = crate::PANICS_CAUGHT.load(std::sync::atomic::Ordering::Relaxed);
    for round in 0..2 {
        for name in &tools {
            for args in &junk {
                survives(&mut s, &t, name, args);
            }
        }
        // Again with the user in the middle of things.
        if round == 0 {
            busy_user(&mut s);
        }
    }
    // Nothing panicked and was caught: a caught panic is a bug even when the answer was an error.
    assert_eq!(crate::PANICS_CAUGHT.load(std::sync::atomic::Ordering::Relaxed), caught);
}

#[test]
fn what_the_user_sees_comes_back_even_when_the_work_aside_panics() {
    let (mut s, _) = mcp_session("mcp-aside-panic", &ALL);
    busy_user(&mut s);
    let before = seen(&s);
    let answer = s.aside(true, |s| {
        s.open_view(View::Unified { role: FolderRole::Inbox });
        assert!(s.compose.is_none() && s.reading.is_none() && s.selection.is_empty());
        panic!("a bug in the work")
    });
    assert_eq!(answer, Err("that call failed unexpectedly".to_string()));
    assert_eq!(seen(&s), before);
    // And an ordinary run gives its answer back, with the user's state as it was.
    assert_eq!(s.aside(false, |s| s.list.len()), Ok(0));
    assert_eq!(seen(&s), before);
}

#[test]
fn the_accounts_are_listed_without_their_servers_or_signatures() {
    let (mut s, t) = mcp_session("mcp-accounts", &ALL);
    let r = tool(&mut s, &t, "mail_accounts", json!({}));
    let accounts = ok(&r)["accounts"].as_array().unwrap();
    assert!(!accounts.is_empty());
    let a = &accounts[0];
    assert!(a["account"].is_string() && a["email"].is_string() && a["status"].is_object() || a["status"].is_string(), "{a}");
    for hidden in ["imap", "smtp", "userAgent", "ews", "graph", "pop"] {
        assert!(a.get(hidden).is_none(), "{hidden}");
    }
    assert!(a["addresses"].as_array().unwrap().iter().all(|i| i.get("signature").is_none() && i["address"].is_string()));
    let inbox = a["folders"].as_array().unwrap().iter().find(|f| f["role"] == "inbox").unwrap();
    assert!(inbox["total"].as_u64().unwrap() >= 5 && inbox["path"] == "INBOX");
    assert_eq!(fails(&tool(&mut s, &t, "mail_accounts", json!({"extra": 1}))), "unknown argument `extra`");
}

#[test]
fn a_folder_is_listed_newest_first_a_page_at_a_time() {
    let (mut s, t) = mcp_session("mcp-list", &ALL);
    let total = inbox(&mut s).len();
    let r = tool(&mut s, &t, "mail_list", json!({"account": "sample", "folder": "INBOX", "limit": 3}));
    let page = ok(&r);
    assert_eq!((page["total"].as_u64().unwrap() as usize, page["offset"].clone()), (total, json!(0)));
    let messages = page["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    let m = &messages[0];
    for field in
        ["key", "date", "from", "to", "subject", "preview", "unread", "flagged", "answered", "forwarded", "pinned", "categories", "attachments"]
    {
        assert!(m.get(field).is_some(), "{field}: {m}");
    }
    let dates: Vec<i64> = messages.iter().map(|m| m["date"].as_i64().unwrap()).collect();
    assert!(dates.windows(2).all(|w| w[0] >= w[1]), "{dates:?}");
    let next = tool(&mut s, &t, "mail_list", json!({"account": "sample", "folder": "INBOX", "limit": 3, "offset": 3}));
    assert_ne!(ok(&next)["messages"][0]["key"], messages[0]["key"]);
    assert_eq!(ok(&next)["offset"], json!(3));
    // Past the end: nothing, not an error.
    let past = tool(&mut s, &t, "mail_list", json!({"account": "sample", "folder": "INBOX", "offset": 100_000}));
    assert_eq!((ok(&past)["messages"].clone(), ok(&past)["total"].as_u64().unwrap() as usize), (json!([]), total));
}

#[test]
fn only_the_unread_can_be_asked_for() {
    let (mut s, t) = mcp_session("mcp-unread", &ALL);
    let keys = inbox(&mut s);
    s.run("list.selectAll", &json!({})).unwrap();
    s.run("mail.markRead", &json!({})).unwrap();
    s.run("list.open", &json!({"key": keys[0].to_id()})).unwrap();
    s.run("mail.markUnread", &json!({})).unwrap();
    let r = tool(&mut s, &t, "mail_list", json!({"account": "sample", "folder": "INBOX", "unread_only": true}));
    let unread = ok(&r)["messages"].as_array().unwrap();
    assert_eq!(unread.len(), 1, "{unread:?}");
    assert_eq!(unread[0]["key"], json!(keys[0].to_id()));
}

#[test]
fn a_list_of_something_that_isnt_there_is_a_tool_error() {
    let (mut s, t) = mcp_session("mcp-list-errors", &ALL);
    let ask = |s: &mut Session, args: Value| fails(&tool(s, &t, "mail_list", args));
    assert!(ask(&mut s, json!({"account": "sample", "folder": "Nowhere"})).contains("no folder Nowhere in sample"));
    assert!(ask(&mut s, json!({"account": "nobody", "folder": "INBOX"})).contains("no account nobody"));
    assert!(ask(&mut s, json!({"account": "sample", "folder": "INBOX", "limit": 0})).contains("`limit`"));
    assert!(ask(&mut s, json!({"account": "sample", "folder": "INBOX", "limit": 201})).contains("`limit`"));
    assert!(ask(&mut s, json!({"account": "sample", "folder": "INBOX", "offset": -1})).contains("`offset`"));
    assert!(ask(&mut s, json!({"folder": "INBOX"})).contains("`account` is required"));
    assert!(ask(&mut s, json!({"account": "sample", "folder": "INBOX", "limit": 1.5})).contains("whole number"));
}

#[test]
fn the_read_tools_leave_what_the_user_sees_as_it_was() {
    let (mut s, t) = mcp_session("mcp-unchanged", &ALL);
    busy_user(&mut s);
    s.run("list.selectAll", &json!({})).unwrap();
    let key = inbox(&mut s)[1].to_id();
    let before = seen(&s);
    let unread_before = s.folder_state("sample", "INBOX").messages.iter().filter(|m| !m.flags.seen).count();
    for (name, args) in [
        ("mail_accounts", json!({})),
        ("mail_list", json!({"account": "sample", "folder": "INBOX"})),
        ("mail_list", json!({"account": "sample", "folder": "Sent"})),
        ("mail_search", json!({"query": "poster"})),
        ("mail_search", json!({"query": "is:unread from:zoe"})),
        ("mail_read", json!({"key": key, "headers": true})),
        ("mail_commands", json!({})),
        ("monitor_log", json!({})),
    ] {
        let _ = tool(&mut s, &t, name, args.clone());
        assert_eq!(seen(&s), before, "{name} {args}");
    }
    assert_eq!(s.folder_state("sample", "INBOX").messages.iter().filter(|m| !m.flags.seen).count(), unread_before);
}

#[test]
fn a_search_finds_mail_without_touching_the_users_own() {
    let (mut s, t) = mcp_session("mcp-search", &ALL);
    s.run_search("Festival", SearchScope::All);
    assert!(!s.list.is_empty());
    let first = s.list[0].key.to_id();
    s.run("list.open", &json!({"key": first})).unwrap();
    let before = seen(&s);
    assert!(before.search.is_some() && before.reading.is_some());
    let r = tool(&mut s, &t, "mail_search", json!({"query": "subject:Storyboard"}));
    let found = ok(&r);
    assert!(found["found"].as_u64().unwrap() >= 2, "{found}");
    assert!(found["messages"].as_array().unwrap().iter().all(|m| m["subject"].as_str().unwrap().contains("Storyboard")));
    assert_eq!(seen(&s), before);
    // The search the user typed is still what a later step shows, and the searched words weren't remembered.
    assert_eq!(s.search.as_ref().map(|(q, _)| q.as_str()), Some("Festival"));
    assert!(!s.prefs.recent_searches.iter().any(|q| q.contains("Storyboard")));
}

#[test]
fn a_search_of_words_in_the_text_finds_from_the_index_as_the_search_box_does() {
    let (mut s, t) = mcp_session("mcp-search-body", &ALL);
    let key = inbox(&mut s)[0].to_id();
    let read = tool(&mut s, &t, "mail_read", json!({"key": key}));
    let text = ok(&read)["text"].as_str().unwrap().to_string();
    let word = text.split(|c: char| !c.is_alphabetic()).filter(|w| w.len() > 5).max_by_key(|w| w.len()).unwrap().to_string();
    let r = tool(&mut s, &t, "mail_search", json!({"query": format!("body:{word}")}));
    let keys: Vec<&str> = ok(&r)["messages"].as_array().unwrap().iter().map(|m| m["key"].as_str().unwrap()).collect();
    assert!(keys.contains(&key.as_str()), "{word}: {keys:?}");
    assert!(fails(&tool(&mut s, &t, "mail_search", json!({"query": ""}))).contains("empty"));
    assert!(fails(&tool(&mut s, &t, "mail_search", json!({"query": "x".repeat(1025)}))).contains("too long"));
}

#[test]
fn reading_a_message_gives_its_text_and_marks_nothing() {
    let (mut s, t) = mcp_session("mcp-read", &ALL);
    s.run("list.selectAll", &json!({})).unwrap();
    s.run("mail.markUnread", &json!({})).unwrap();
    let key = inbox(&mut s)[0].to_id();
    let before = seen(&s);
    let r = tool(&mut s, &t, "mail_read", json!({"key": key}));
    let m = ok(&r);
    assert_eq!(m["key"], json!(key));
    assert!(m["from"].as_str().unwrap().contains('@') && !m["subject"].as_str().unwrap().is_empty() && !m["text"].as_str().unwrap().is_empty());
    assert_eq!((m["truncated"].clone(), m["external"].is_boolean(), m["attachments"].is_array()), (json!(false), true, true));
    assert!(m.get("headers").is_none());
    // It's still unread, and the pane shows what it showed.
    assert!(s.folder_state("sample", "INBOX").messages.iter().all(|m| !m.flags.seen));
    assert_eq!(seen(&s), before);
    let with = tool(&mut s, &t, "mail_read", json!({"key": key, "headers": true}));
    let h = &ok(&with)["headers"];
    assert!(h["raw"].as_str().unwrap().contains("Subject:") && h["authenticationResults"].is_array());
    assert!(["confirmed", "failed", "unknown"].contains(&h["senderCheck"].as_str().unwrap()));
}

#[test]
fn a_message_that_isnt_there_or_a_key_that_isnt_one_is_a_tool_error() {
    let (mut s, t) = mcp_session("mcp-read-errors", &ALL);
    assert_eq!(fails(&tool(&mut s, &t, "mail_read", json!({"key": "sample:1:999:INBOX"}))), "no such message");
    assert_eq!(fails(&tool(&mut s, &t, "mail_read", json!({"key": "gibberish"}))), "`key` isn't a message key");
    assert_eq!(fails(&tool(&mut s, &t, "mail_read", json!({"key": "nobody:1:1:INBOX"}))), "no such message");
    assert!(fails(&tool(&mut s, &t, "mail_read", json!({}))).contains("`key` is required"));
    assert!(fails(&tool(&mut s, &t, "mail_read", json!({"key": "sample:1:1:INBOX", "headers": "yes"}))).contains("true or false"));
}

/// A session over Projects with messages that have no source yet, and its first message's key.
fn without_bodies(name: &str) -> (Session, String, MessageKey) {
    let mut s = big_projects(&temp(name), 3);
    let t = token();
    s.mcp_seed_token(&t, &ALL);
    let key = s.list.first().map(|m| m.key.clone()).unwrap();
    (s, t, key)
}

fn ask_read(s: &mut Session, t: &str, key: &MessageKey, now: i64) -> McpOutcome {
    s.mcp_handle(t, &request(1, "tools/call", json!({"name": "mail_read", "arguments": {"key": key.to_id()}})), now)
}

fn raw_of(key: &MessageKey) -> Vec<u8> {
    format!("From: Ann <ann@example.com>\r\nTo: zoe@example.org\r\nSubject: Report {}\r\n\r\nThe figures.\r\n", key.uid).into_bytes()
}

#[test]
fn a_message_not_downloaded_is_asked_for_and_answered_when_it_comes() {
    let (mut s, t, key) = without_bodies("mcp-read-later");
    let McpOutcome::Later(ticket) = ask_read(&mut s, &t, &key, 10_000) else { panic!("it should wait") };
    assert!(s.take_jobs().iter().any(|j| matches!(j, Job::FetchBody { key: k } if *k == key)));
    assert!(s.mcp_finished(11_000).is_empty());
    let raw = raw_of(&key);
    s.store.as_ref().unwrap().write_bodies(vec![(key.clone(), raw.clone())]).unwrap();
    s.apply(Event::Body { key: key.clone(), raw });
    let done = s.mcp_finished(12_000);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].0, ticket);
    let line: Value = serde_json::from_str(&done[0].1).unwrap();
    assert!(ok(&line["result"])["text"].as_str().unwrap().contains("The figures"), "{line}");
    assert!(!s.mcp_waiting());
    // The token may call again.
    assert_eq!(reply(s.mcp_handle(&t, &request(2, "ping", json!({})), 20_000))["result"], json!({}));
}

#[test]
fn a_body_that_fails_or_doesnt_come_in_time_is_a_tool_error() {
    let (mut s, t, key) = without_bodies("mcp-read-fails");
    assert!(matches!(ask_read(&mut s, &t, &key, 10_000), McpOutcome::Later(_)));
    s.apply(Event::BodyFailed { key: key.clone(), error: "the server said no".into() });
    let done = s.mcp_finished(11_000);
    let line: Value = serde_json::from_str(&done[0].1).unwrap();
    assert!(fails(&line["result"]).contains("the server said no"));
    // Another message that never comes: out of time after 60 s.
    let other = s.list[1].key.clone();
    assert!(matches!(ask_read(&mut s, &t, &other, 100_000), McpOutcome::Later(_)));
    assert!(s.mcp_finished(159_999).is_empty());
    let done = s.mcp_finished(160_000);
    let line: Value = serde_json::from_str(&done[0].1).unwrap();
    assert!(fails(&line["result"]).contains("didn't download in time"));
}

#[test]
fn a_page_with_pictures_from_the_web_fetches_none_of_them() {
    let (mut s, t, key) = without_bodies("mcp-read-html");
    let raw = b"From: Ann <ann@example.com>\r\nTo: zoe@example.org\r\nSubject: Pictures\r\nMIME-Version: 1.0\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>Look at this</p><img src=\"https://example.com/p.png\"><img src=\"https://example.com/track.gif\" width=1>".to_vec();
    s.store.as_ref().unwrap().write_bodies(vec![(key.clone(), raw)]).unwrap();
    let r = tool(&mut s, &t, "mail_read", json!({"key": key.to_id()}));
    assert!(ok(&r)["text"].as_str().unwrap().contains("Look at this"));
    assert!(!s.take_jobs().iter().any(|j| matches!(j, Job::FetchUrl { .. })));
    assert!(s.pictures.is_empty());
}

#[test]
fn a_big_header_block_is_cut_to_64_kib_at_a_character() {
    let (mut s, t, key) = without_bodies("mcp-read-headers");
    let mut raw = String::from("From: Ann <ann@example.com>\r\nTo: zoe@example.org\r\nSubject: Headers\r\n");
    while raw.len() < 200 << 10 {
        raw.push_str(&format!("X-Pad-{}: {}\r\n", raw.len(), "é".repeat(60)));
    }
    raw.push_str("\r\nBody.\r\n");
    s.store.as_ref().unwrap().write_bodies(vec![(key.clone(), raw.into_bytes())]).unwrap();
    let r = tool(&mut s, &t, "mail_read", json!({"key": key.to_id(), "headers": true}));
    let head = ok(&r)["headers"]["raw"].as_str().unwrap();
    assert!(head.len() <= 64 << 10 && head.len() > 60 << 10, "{}", head.len());
    assert!(head.starts_with("From: Ann"));
}

#[test]
fn only_the_commands_on_the_list_run_and_nothing_happens_for_the_others() {
    let (mut s, t) = mcp_session("mcp-run-denied", &ALL);
    let key = inbox(&mut s)[0].to_id();
    let before = (seen(&s), s.folder_state("sample", "INBOX").clone());
    for id in [
        "mail.delete",
        "mail.archive",
        "rules.add",
        "account.remove",
        "mcp.createToken",
        "ui.options",
        "folder.empty",
        "mail.moveToFocused",
        "no.such",
    ] {
        let r = tool(&mut s, &t, "mail_run", json!({"command": id, "keys": [key.clone()]}));
        assert_eq!(fails(&r), format!("{id} isn't one an AI app may run"));
    }
    assert_eq!((seen(&s), s.folder_state("sample", "INBOX").clone()), before);
    assert!(s.take_jobs().is_empty());
}

#[test]
fn moves_to_trash_or_junk_or_across_accounts_are_refused() {
    let (mut s, t) = mcp_session("mcp-run-move", &ALL);
    let key = inbox(&mut s)[0].to_id();
    let folders = s.account("sample").unwrap().folders.clone();
    let path_of = |role: FolderRole| folders.iter().find(|f| f.role == role).map(|f| f.path.clone()).unwrap();
    let before = s.folder_state("sample", "INBOX").clone();
    for params in [
        json!({"folder": "role:trash"}),
        json!({"folder": "role:junk"}),
        json!({"folder": path_of(FolderRole::Trash)}),
        json!({"folder": path_of(FolderRole::Junk)}),
        json!({"folder": "Projects", "account": "sample"}),
        json!({}),
    ] {
        let r = tool(&mut s, &t, "mail_run", json!({"command": "mail.move", "keys": [key.clone()], "params": params}));
        assert!(r["isError"] == json!(true), "{params}: {r}");
    }
    assert_eq!(*s.folder_state("sample", "INBOX"), before);
    // A move to an ordinary folder is allowed.
    let r = tool(&mut s, &t, "mail_run", json!({"command": "mail.move", "keys": [key.clone()], "params": {"folder": "Projects"}}));
    assert_eq!(r["isError"], json!(false), "{r}");
    assert!(s.folder_state("sample", "Projects").messages.iter().any(|m| m.subject == before.messages[0].subject));
}

#[test]
fn a_category_has_to_be_one_that_is_in_the_list() {
    let (mut s, t) = mcp_session("mcp-run-category", &ALL);
    let key = inbox(&mut s)[0].to_id();
    for params in [json!({"category": "Not a category"}), json!({"keyword": "$label6"}), json!({"category": "Red", "keyword": "$label1"}), json!({})]
    {
        let r = tool(&mut s, &t, "mail_run", json!({"command": "mail.categorize", "keys": [key.clone()], "params": params}));
        assert_eq!(r["isError"], json!(true), "{params}");
    }
    let name = s.prefs.categories[0].name.clone();
    let r = tool(&mut s, &t, "mail_run", json!({"command": "mail.categorize", "keys": [key.clone()], "params": {"category": name}}));
    assert_eq!(r["isError"], json!(false), "{r}");
}

#[test]
fn keys_have_to_be_from_one_folder_and_all_there() {
    let (mut s, t) = mcp_session("mcp-run-keys", &ALL);
    let inbox_key = inbox(&mut s)[0].to_id();
    let other = s.folder_state("sample", "Projects/Short film").messages[0].key.to_id();
    let run = |s: &mut Session, keys: Value| tool(s, &t, "mail_run", json!({"command": "mail.markRead", "keys": keys}));
    let before = s.folder_state("sample", "INBOX").clone();
    assert_eq!(fails(&run(&mut s, json!([inbox_key.clone(), other]))), "keys from one folder at a time");
    assert!(fails(&run(&mut s, json!([inbox_key.clone(), "sample:1:99999:INBOX"]))).contains("1 of the keys aren't in that folder"));
    assert!(fails(&run(&mut s, json!(["rubbish"]))).contains("isn't a message key"));
    assert!(fails(&run(&mut s, json!([]))).contains("`keys`"));
    let many: Vec<String> = (0..101).map(|i| format!("sample:1:{i}:INBOX")).collect();
    assert!(fails(&run(&mut s, json!(many))).contains("`keys`"));
    assert_eq!(*s.folder_state("sample", "INBOX"), before);
}

#[test]
fn a_command_run_for_an_app_lands_without_touching_the_users_view_and_can_be_undone() {
    let (mut s, t) = mcp_session("mcp-run-lands", &ALL);
    let keys = inbox(&mut s);
    // Make two unread, then the user goes to Projects and reads a message there.
    s.run("list.selectAll", &json!({})).unwrap();
    s.run("mail.markUnread", &json!({})).unwrap();
    busy_user(&mut s);
    let before = seen(&s);
    let ids = [keys[0].to_id(), keys[1].to_id()];
    let r = tool(&mut s, &t, "mail_run", json!({"command": "mail.markRead", "keys": ids}));
    assert_eq!(r["isError"], json!(false), "{r}");
    let read: Vec<bool> = s.folder_state("sample", "INBOX").messages.iter().take(3).map(|m| m.flags.seen).collect();
    assert_eq!(read, [true, true, false]);
    assert_eq!(seen(&s), before);
    // The owner's Ctrl+Z undoes what the app did.
    s.run("edit.undo", &json!({})).unwrap();
    assert!(s.folder_state("sample", "INBOX").messages.iter().take(2).all(|m| !m.flags.seen));
}

#[test]
fn a_sample_mailbox_has_no_server_to_fetch_from_and_offline_mail_isnt_fetched() {
    let (mut s, t) = mcp_session("mcp-sync-none", &ALL);
    assert!(fails(&tool(&mut s, &t, "mail_sync", json!({}))).contains("no account with a server"));
    assert!(fails(&tool(&mut s, &t, "mail_sync", json!({"account": "sample"}))).contains("sample mailbox has no server"));
    assert!(fails(&tool(&mut s, &t, "mail_sync", json!({"account": "nobody"}))).contains("no account nobody"));
    s.prefs.work_offline = true;
    assert_eq!(fails(&tool(&mut s, &t, "mail_sync", json!({}))), "MailCraft is working offline");
    assert!(s.take_jobs().is_empty());
}

fn syncing(name: &str) -> (Session, String, String) {
    let (mut s, id) = server_account(name, json!({"email": "zoe@example.org", "password": "pw"}), &[("INBOX", FolderRole::Inbox)]);
    s.take_jobs();
    let t = token();
    s.mcp_seed_token(&t, &ALL);
    (s, t, id)
}

fn ask_sync(s: &mut Session, t: &str, now: i64) -> McpOutcome {
    s.mcp_handle(t, &request(1, "tools/call", json!({"name": "mail_sync", "arguments": {}})), now)
}

#[test]
fn a_sync_waits_for_every_account_to_answer() {
    let (mut s, t, id) = syncing("mcp-sync");
    assert!(matches!(ask_sync(&mut s, &t, 10_000), McpOutcome::Later(_)));
    assert!(s.take_jobs().iter().any(|j| matches!(j, Job::SyncAll { account } if *account == id)));
    s.apply(status(&id, SyncStatus::Syncing { folder: "INBOX".into() }));
    assert!(s.mcp_finished(11_000).is_empty());
    s.apply(status(&id, SyncStatus::Online { at: NOW }));
    let done = s.mcp_finished(12_000);
    let line: Value = serde_json::from_str(&done[0].1).unwrap();
    assert_eq!(ok(&line["result"]), &json!({"done": [id], "running": [], "failed": []}));
    assert!(!s.mcp_waiting());
}

#[test]
fn an_account_that_cant_sign_in_is_reported_failed_and_one_that_is_silent_running() {
    let (mut s, t, id) = syncing("mcp-sync-failed");
    assert!(matches!(ask_sync(&mut s, &t, 10_000), McpOutcome::Later(_)));
    s.apply(status(&id, SyncStatus::NeedsPassword { error: "the password was refused".into() }));
    let line: Value = serde_json::from_str(&s.mcp_finished(11_000)[0].1).unwrap();
    assert_eq!(ok(&line["result"])["failed"], json!([{"account": id, "error": "the password was refused"}]));
    assert_eq!(ok(&line["result"])["done"], json!([]));
    // Silence: at the deadline it's still running.
    assert!(matches!(ask_sync(&mut s, &t, 200_000), McpOutcome::Later(_)));
    assert!(s.mcp_finished(319_999).is_empty());
    let line: Value = serde_json::from_str(&s.mcp_finished(320_000)[0].1).unwrap();
    assert_eq!(ok(&line["result"])["running"], json!([id]));
}

fn fake_log(q: &LogQuery) -> Vec<LogLine> {
    let all = [
        LogLine { at: 100, level: "WARN".into(), target: "mailcraft::net".into(), text: "a server was slow".into(), panic: false },
        LogLine { at: 200, level: "ERROR".into(), target: "mailcraft::net".into(), text: "a connection failed".into(), panic: false },
        LogLine { at: 300, level: "ERROR".into(), target: "mailcraft".into(), text: "internal error: a panic".into(), panic: true },
    ];
    all.into_iter().filter(|l| q.since.is_none_or(|s| l.at >= s) && (!q.errors_only || l.level == "ERROR")).take(q.limit).collect()
}

#[test]
fn the_log_is_read_through_the_apps_source() {
    let (mut s, t) = mcp_session("mcp-log", &ALL);
    assert_eq!(fails(&tool(&mut s, &t, "monitor_log", json!({}))), "this MailCraft keeps no log");
    s.mcp.log_source = Some(fake_log);
    let all = tool(&mut s, &t, "monitor_log", json!({}));
    assert_eq!(ok(&all)["lines"].as_array().unwrap().len(), 3);
    let errors = tool(&mut s, &t, "monitor_log", json!({"errors_only": true, "limit": 1}));
    assert_eq!(ok(&errors)["lines"].as_array().unwrap().len(), 1);
    assert_eq!(ok(&tool(&mut s, &t, "monitor_log", json!({"since": 250})))["lines"][0]["panic"], json!(true));
    assert_eq!(ok(&tool(&mut s, &t, "monitor_log", json!({"since": 9_999_999_999i64})))["lines"], json!([]));
    assert!(fails(&tool(&mut s, &t, "monitor_log", json!({"limit": 501}))).contains("`limit`"));
    assert!(fails(&tool(&mut s, &t, "monitor_log", json!({"limit": 0}))).contains("`limit`"));
}

#[test]
fn the_log_says_who_and_which_tool_never_what() {
    // Installed before the calls, so their lines are kept (on this thread).
    let _ = mcp_logged();
    let (mut s, t) = mcp_session("mcp-log-privacy", &ALL);
    let key = inbox(&mut s)[0].to_id();
    let subject = s.folder_state("sample", "INBOX").messages[0].subject.clone();
    for (name, args) in [
        ("mail_accounts", json!({})),
        ("mail_list", json!({"account": "sample", "folder": "INBOX"})),
        ("mail_search", json!({"query": "poster"})),
        ("mail_read", json!({"key": key.clone()})),
        ("mail_run", json!({"command": "mail.markRead", "keys": [key.clone()]})),
        ("mail_sync", json!({})),
        ("mail_read", json!({"key": "nobody:1:1:INBOX"})),
    ] {
        let _ = tool(&mut s, &t, name, args);
    }
    let _ = call(&mut s, "mcp_wrongwrongwrongwrongwrongwrongwrongwrongwro", "ping", json!({}));
    let lines = mcp_logged();
    assert!(lines.iter().any(|l| l.starts_with("test: mail_list ok in ")), "{lines:?}");
    for line in &lines {
        assert!(!line.contains(&t) && !line.contains('@') && !line.contains(&key) && !line.contains(&subject) && !line.contains("poster"), "{line}");
    }
}
