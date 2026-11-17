//! `mail_send`: the rules for sending without a code, messages held for one, and the code.

use mailcraft_model::{EmailAddress, Flags, Summary};

use super::*;
use crate::mcp::approval::{self, ApprovalRecord, ApprovalState};

/// RFC 6238's secret: codes for it are worked out in the tests.
const SECRET: &[u8] = b"12345678901234567890";

fn activate(s: &mut Session) {
    s.mcp.settings.approval = ApprovalRecord { state: ApprovalState::Active, ..Default::default() };
    s.mcp.secret = Some(SECRET.to_vec());
}

/// The code for the step of `now_ms`.
fn code_for(now_ms: i64) -> String {
    format!("{:06}", approval::code_at(SECRET, approval::step(now_ms.div_euclid(1000))).unwrap())
}

fn summary(folder: &str, uid: u32, id: &str, from: (&str, &str), to: &[(&str, &str)], subject: &str) -> Summary {
    Summary {
        key: MessageKey { account: "sample".into(), folder: folder.into(), validity: 1, uid },
        message_id: id.into(),
        subject: subject.into(),
        from: EmailAddress::new(from.0, from.1),
        to: to.iter().map(|t| EmailAddress::new(t.0, t.1)).collect(),
        date: NOW - 3600,
        flags: Flags { seen: true, ..Default::default() },
        ..Default::default()
    }
}

fn raw_from(id: &str, from: &str, subject: &str) -> Vec<u8> {
    format!("From: {from}\r\nTo: riley@studio.example\r\nSubject: {subject}\r\nMessage-ID: <{id}>\r\n\r\nPlease answer.\r\n").into_bytes()
}

/// The sample, with the owner having answered the print shop's proof (in Sent, in its thread), a
/// second message from the print shop that nothing of the owner's answers, and a message in the
/// Inbox that says it's from the owner to a stranger.
struct Fixture {
    s: Session,
    t: String,
    /// The print shop's message the owner has written in the thread of.
    thread: MessageKey,
    /// The print shop's other message.
    other: MessageKey,
    /// Zoe's, in the Inbox.
    zoe: MessageKey,
}

fn fixture(name: &str) -> Fixture {
    let store = temp(name);
    assert!(sample::seed_if_empty(&store, NOW).unwrap());
    let mut inbox = store.load_folder("sample", "INBOX").unwrap();
    let thread = inbox.messages.iter().find(|m| m.from.email == "orders@printshop.example").cloned().unwrap();
    let zoe = inbox.messages.iter().find(|m| m.from.email == "zoe@studio.example").cloned().unwrap();
    let mut sent = store.load_folder("sample", "Sent").unwrap();
    let mut mine = summary(
        "Sent",
        99,
        "owner-reply@studio.example",
        ("Riley Moreno", "riley@studio.example"),
        &[("Print Shop", "orders@printshop.example")],
        "Re: proof",
    );
    mine.in_reply_to = thread.message_id.clone();
    mine.references = vec![thread.message_id.clone()];
    sent.messages.insert(0, mine.into());
    store.save_folder("sample", "Sent", &sent).unwrap();
    let other = summary(
        "INBOX",
        98,
        "other@printshop.example",
        ("Print Shop", "orders@printshop.example"),
        &[("Riley", "riley@studio.example")],
        "Another proof",
    );
    let forged =
        summary("INBOX", 97, "forged@example.com", ("Riley Moreno", "riley@studio.example"), &[("Evil", "evil@example.com")], "Totally from me");
    store.write_bodies(vec![(other.key.clone(), raw_from("other@printshop.example", "orders@printshop.example", "Another proof"))]).unwrap();
    let other_key = other.key.clone();
    inbox.messages.insert(0, other.into());
    inbox.messages.insert(0, forged.into());
    store.save_folder("sample", "INBOX", &inbox).unwrap();
    let mut s = Session::open(Some(store), mailcraft_model::dates::Clock::new(NOW, 0));
    let t = token();
    s.mcp_seed_token(&t, &ALL);
    s.mcp.random = Some(|n| Some(vec![0x5a; n]));
    Fixture { s, t, thread: thread.key.clone(), other: other_key, zoe: zoe.key.clone() }
}

/// What `mail_send` answered, as the result's `structuredContent`.
fn answered(r: &Value) -> &Value {
    ok(r)
}

fn reasons(r: &Value) -> Vec<String> {
    answered(r)["summary"]["reasons"].as_array().unwrap().iter().map(|x| x["rule"].as_str().unwrap().to_string()).collect()
}

#[test]
fn a_reply_that_passes_every_rule_goes_without_a_code() {
    let Fixture { mut s, t, thread, .. } = fixture("mcp-send-auto");
    busy_user(&mut s);
    let before = seen(&s);
    let r = tool(&mut s, &t, "mail_send", json!({"in_reply_to": thread.to_id(), "body": "Approved, thank you."}));
    assert_eq!(answered(&r)["outcome"], json!("sent"), "{r}");
    assert!(answered(&r)["id"].is_number());
    // The user's own message, pane and view are as they were, and no window opened.
    assert_eq!(seen(&s), before);
    assert!(s.composes.is_empty());
    // It's in Sent, replies above the quote, to the print shop.
    let sent = s.folder_state("sample", "Sent").messages.iter().find(|m| m.subject.starts_with("Re: Your proof")).cloned().unwrap();
    assert_eq!(sent.to[0].email, "orders@printshop.example");
    let raw = s.store.as_ref().unwrap().read_body(&sent.key).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap();
    assert!(raw.contains("Approved, thank you.") && raw.contains("In-Reply-To:"), "{raw}");
    assert_eq!(s.mcp.settings.sent_today.1, 1);
    // The original earned its Replied flag.
    assert!(s.folder_state("sample", "INBOX").messages.iter().find(|m| m.key == thread).unwrap().flags.answered);
}

#[test]
fn a_new_message_needs_a_code_even_to_someone_written_to() {
    let Fixture { mut s, t, .. } = fixture("mcp-send-new");
    activate(&mut s);
    let r = tool(&mut s, &t, "mail_send", json!({"to": "orders@printshop.example", "subject": "Hello", "body": "Hi"}));
    assert_eq!(answered(&r)["outcome"], json!("needsCode"), "{r}");
    assert_eq!(reasons(&r), ["notAReply"]);
    let summary = &answered(&r)["summary"];
    assert_eq!(
        (summary["to"].clone(), summary["subject"].clone(), summary["from"].clone()),
        (json!(["orders@printshop.example"]), json!("Hello"), json!("riley@studio.example"))
    );
    assert!(summary["size"].as_u64().unwrap() > 100 && summary["attachments"] == json!([]));
    assert_eq!(answered(&r)["pendingId"], json!("5a5a5a5a5a5a5a5a"));
    assert!(answered(&r)["expiresAt"].is_number());
    assert_eq!(s.mcp.pending.len(), 1);
    assert!(s.notices.iter().any(|n| n.text.contains("An AI app asks to send a message")));
    assert_eq!(s.mcp.settings.sent_today.1, 0);
    assert!(s.folder_state("sample", "Sent").messages.iter().all(|m| m.subject != "Hello"));
}

#[test]
fn each_rule_broken_on_its_own_says_which() {
    let Fixture { mut s, t, thread, other, .. } = fixture("mcp-send-rules");
    activate(&mut s);
    let reply = |s: &mut Session, key: &MessageKey, extra: Value| {
        let mut args = json!({"in_reply_to": key.to_id(), "body": "ok"});
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        // Each case on its own: what an earlier one held doesn't count towards the five.
        s.mcp.pending.clear();
        tool(s, &t, "mail_send", args)
    };
    // The owner hasn't written in this conversation.
    assert_eq!(reasons(&reply(&mut s, &other, json!({}))), ["ownerNotInThread"]);
    // From another account.
    let second = s.run("account.add", &json!({"email": "me@example.org", "local": true})).unwrap()["account"].as_str().unwrap().to_string();
    assert_eq!(reasons(&reply(&mut s, &thread, json!({"from": second}))), ["otherAccount"]);
    // A recipient never written to.
    let r = reply(&mut s, &thread, json!({"to": "stranger@example.com"}));
    assert_eq!(answered(&r)["summary"]["reasons"], json!([{"rule": "unknownRecipients", "count": 1}]));
    // A file.
    let file = scratch("mcp-send-file");
    std::fs::create_dir_all(&file).unwrap();
    let path = file.join("notes.txt");
    std::fs::write(&path, "x").unwrap();
    let r = reply(&mut s, &thread, json!({"attachments": [path.to_string_lossy()]}));
    assert_eq!(reasons(&r), ["hasAttachments"]);
    assert_eq!(answered(&r)["summary"]["attachments"], json!([{"name": "notes.txt", "size": 1}]));
    // The limit for the day.
    s.mcp.settings.sent_today = (mailcraft_model::dates::today(&s.clock), 20);
    assert_eq!(reasons(&reply(&mut s, &thread, json!({}))), ["dailyLimit"]);
    s.mcp.settings.sent_today = (0, 0);
    // The rules switched off.
    s.mcp.settings.rules.no_code_replies = false;
    assert_eq!(reasons(&reply(&mut s, &thread, json!({}))), ["rulesOff"]);
    s.mcp.settings.rules.no_code_replies = true;
    // And a reply that breaks none still goes (the control).
    assert_eq!(answered(&reply(&mut s, &thread, json!({})))["outcome"], json!("sent"));
}

#[test]
fn a_look_alike_address_or_many_outside_organisations_hold_a_reply() {
    let Fixture { mut s, t, thread, .. } = fixture("mcp-send-checks");
    activate(&mut s);
    let r = tool(&mut s, &t, "mail_send", json!({"in_reply_to": thread.to_id(), "body": "ok", "to": "orders@printsh0p.example"}));
    assert!(reasons(&r).contains(&"sendChecks".to_string()), "{r}");
    assert!(answered(&r)["summary"]["warnings"][0]["address"].is_string());
    let many: Vec<String> = (0..8).map(|i| format!("p{i}@org{i}.example")).collect();
    let r = tool(&mut s, &t, "mail_send", json!({"in_reply_to": thread.to_id(), "body": "ok", "to": many}));
    assert!(reasons(&r).contains(&"sendChecks".to_string()), "{r}");
}

#[test]
fn a_message_in_the_inbox_that_says_its_from_the_owner_makes_nobody_known() {
    let Fixture { mut s, t, thread, .. } = fixture("mcp-send-forged");
    activate(&mut s);
    // The inbox holds a message "from the owner" to evil@example.com: the address book ContactBook
    // counts would take it as mail the owner wrote.
    let r = tool(&mut s, &t, "mail_send", json!({"in_reply_to": thread.to_id(), "body": "ok", "to": "evil@example.com"}));
    assert_eq!(answered(&r)["summary"]["reasons"], json!([{"rule": "unknownRecipients", "count": 1}]));
    // Nor does moving it into Sent: an AI app may not move mail there.
    let forged = s.folder_state("sample", "INBOX").messages[0].key.to_id();
    let r = tool(&mut s, &t, "mail_run", json!({"command": "mail.move", "keys": [forged], "params": {"folder": "Sent"}}));
    assert!(fails(&r).contains("Trash, Junk or Sent"), "{r}");
    let forged = s.folder_state("sample", "INBOX").messages[0].key.to_id();
    let r = tool(&mut s, &t, "mail_run", json!({"command": "mail.move", "keys": [forged], "params": {"folder": "role:sent"}}));
    assert!(fails(&r).contains("Trash, Junk or Sent"), "{r}");
}

#[test]
fn a_saved_contact_counts_as_known() {
    let Fixture { mut s, t, thread, .. } = fixture("mcp-send-contact");
    s.pim.entry("sample".into()).or_default().cards.push(mailcraft_model::pim::StoredCard {
        card: mailcraft_model::vcard::Card {
            name: "Dana Cole".into(),
            emails: vec![mailcraft_model::vcard::Typed { value: "Dana@Example.com".into(), kind: String::new() }],
            ..Default::default()
        },
        ..Default::default()
    });
    let r = tool(&mut s, &t, "mail_send", json!({"in_reply_to": thread.to_id(), "body": "ok", "cc": "dana@example.com"}));
    assert_eq!(answered(&r)["outcome"], json!("sent"), "{r}");
}

#[test]
fn what_cant_be_sent_is_refused_or_a_tool_error() {
    let Fixture { mut s, t, thread, zoe, .. } = fixture("mcp-send-refused");
    let ask = |s: &mut Session, args: Value| tool(s, &t, "mail_send", args);
    // Eleven recipients.
    let eleven: Vec<String> = (0..11).map(|i| format!("p{i}@example.com")).collect();
    assert!(fails(&ask(&mut s, json!({"to": eleven, "subject": "s", "body": "b"}))).contains("more than 10 recipients"));
    // Not an address; nobody to send to; nothing to say.
    assert!(fails(&ask(&mut s, json!({"to": "not an address", "subject": "s", "body": "b"}))).contains("not an email address"));
    assert!(fails(&ask(&mut s, json!({"subject": "s", "body": "b"}))).contains("`to` is required"));
    assert!(fails(&ask(&mut s, json!({"to": "a@example.com", "subject": "s"}))).contains("`body` is required"));
    assert!(fails(&ask(&mut s, json!({"to": "a@example.com", "body": "b", "bogus": 1}))).contains("unknown argument"));
    // A reply to a message that isn't there.
    assert!(fails(&ask(&mut s, json!({"in_reply_to": "sample:1:9999:INBOX", "body": "b"}))).contains("no such message"));
    assert!(fails(&ask(&mut s, json!({"in_reply_to": "junk", "body": "b"}))).contains("isn't a message key"));
    // A code is needed and approval isn't set up.
    let r = ask(&mut s, json!({"to": "a@example.com", "subject": "s", "body": "b"}));
    assert!(fails(&r).contains("approval isn't set up"), "{r}");
    // Locked.
    activate(&mut s);
    s.mcp.settings.approval.locked = true;
    assert!(fails(&ask(&mut s, json!({"in_reply_to": thread.to_id(), "body": "b"}))).contains("sending is locked"));
    s.mcp.settings.approval.locked = false;
    // Too many held.
    for i in 0..5 {
        let r = ask(&mut s, json!({"to": format!("p{i}@example.com"), "subject": "s", "body": "b"}));
        assert_eq!(answered(&r)["outcome"], json!("needsCode"), "{i}: {r}");
        s.mcp.random = Some(|n| Some(vec![0x6b; n]));
    }
    assert!(fails(&ask(&mut s, json!({"to": "q@example.com", "subject": "s", "body": "b"}))).contains("too many sends"));
    // A token without the send permission isn't offered the tool and can't call it.
    let (mut s2, t2) = mcp_session("mcp-send-noperm", &[Permission::Read, Permission::Organize]);
    assert_eq!(fails(&tool(&mut s2, &t2, "mail_send", json!({"to": "a@example.com", "body": "b"}))), "this token may not use mail_send");
    let listed = call(&mut s2, &t2, "tools/list", json!({}));
    assert!(listed["result"]["tools"].as_array().unwrap().iter().all(|x| x["name"] != "mail_send"));
    // A reply whose original hasn't been downloaded says to read it first.
    let mut s3 = big_projects(&temp("mcp-send-undownloaded"), 2);
    let t3 = token();
    s3.mcp_seed_token(&t3, &ALL);
    let key = s3.list[0].key.to_id();
    assert!(fails(&tool(&mut s3, &t3, "mail_send", json!({"in_reply_to": key, "body": "b"}))).contains("read it first with mail_read"));
    let _ = zoe;
}

#[test]
fn the_right_code_releases_exactly_the_message_that_was_held() {
    let Fixture { mut s, t, .. } = fixture("mcp-send-approve");
    activate(&mut s);
    let held = tool(&mut s, &t, "mail_send", json!({"to": "a@example.com", "subject": "Original subject", "body": "Original body"}));
    let pending = answered(&held)["pendingId"].as_str().unwrap().to_string();
    let now = tick();
    // Nothing else can ride along with the code.
    let r = tool_at(&mut s, &t, "mail_send", json!({"pending_id": pending, "approval_code": code_for(now), "to": "b@example.com"}), now);
    assert!(fails(&r).contains("nothing else"), "{r}");
    assert_eq!(s.mcp.pending.len(), 1);
    // A wrong code.
    let r = tool_at(&mut s, &t, "mail_send", json!({"pending_id": pending, "approval_code": "000000"}), now);
    assert_eq!(fails(&r), "that code isn't right");
    assert_eq!(s.mcp.settings.approval.failures, 1);
    // Not six digits.
    assert!(fails(&tool_at(&mut s, &t, "mail_send", json!({"pending_id": pending, "approval_code": "12"}), now)).contains("six digits"));
    // The right one.
    let r = tool_at(&mut s, &t, "mail_send", json!({"pending_id": pending, "approval_code": code_for(now)}), now);
    assert_eq!(answered(&r)["outcome"], json!("sent"), "{r}");
    assert!(s.mcp.pending.is_empty());
    let sent = s.folder_state("sample", "Sent").messages.iter().find(|m| m.subject == "Original subject").cloned().unwrap();
    let raw = String::from_utf8_lossy(&s.store.as_ref().unwrap().read_body(&sent.key).unwrap()).into_owned();
    assert!(raw.contains("Original body") && raw.contains("a@example.com"));
    // An approved send isn't one of the day's automatic ones.
    assert_eq!(s.mcp.settings.sent_today.1, 0);
    // The same code can't release another: its step was taken.
    let again = tool(&mut s, &t, "mail_send", json!({"to": "c@example.com", "subject": "Second", "body": "b"}));
    let pending2 = answered(&again)["pendingId"].as_str().unwrap().to_string();
    let r = tool_at(&mut s, &t, "mail_send", json!({"pending_id": pending2, "approval_code": code_for(now)}), now);
    assert_eq!(fails(&r), "that code isn't right");
}

#[test]
fn a_code_works_only_for_the_token_that_asked_and_before_the_send_expires() {
    let Fixture { mut s, t, .. } = fixture("mcp-send-owner-token");
    activate(&mut s);
    let other_token = crate::mcp::tokens::make_token(&[9u8; 32]).unwrap();
    s.mcp_seed_token(&other_token, &ALL);
    let held = tool_at(&mut s, &t, "mail_send", json!({"to": "a@example.com", "subject": "s", "body": "b"}), 5_000_000);
    let pending = answered(&held)["pendingId"].as_str().unwrap().to_string();
    // Another token can't approve it, and isn't told whether it exists.
    let now = 5_100_000;
    let r = tool_at(&mut s, &other_token, "mail_send", json!({"pending_id": pending, "approval_code": code_for(now)}), now);
    assert_eq!(fails(&r), "no such pending send: it may have expired");
    assert_eq!(s.mcp.settings.approval.failures, 0);
    // Ten minutes and a millisecond after it was held it's gone.
    let late = 5_000_000 + 600_001;
    let r = tool_at(&mut s, &t, "mail_send", json!({"pending_id": pending, "approval_code": code_for(late)}), late);
    assert_eq!(fails(&r), "no such pending send: it may have expired");
    assert!(s.mcp.pending.is_empty());
}

#[test]
fn five_wrong_codes_lock_sending_until_the_owner_unlocks_it() {
    let Fixture { mut s, t, .. } = fixture("mcp-send-lock");
    activate(&mut s);
    let held = tool(&mut s, &t, "mail_send", json!({"to": "a@example.com", "subject": "s", "body": "b"}));
    let pending = answered(&held)["pendingId"].as_str().unwrap().to_string();
    let now = tick();
    for n in 1..=5 {
        let r = tool_at(&mut s, &t, "mail_send", json!({"pending_id": pending, "approval_code": "000000"}), now + n);
        assert!(fails(&r).contains("that code isn't right"), "{n}");
    }
    assert!(s.mcp.settings.approval.locked);
    // The right code is refused now, and nothing new can be sent.
    let r = tool_at(&mut s, &t, "mail_send", json!({"pending_id": pending, "approval_code": code_for(now)}), now + 6);
    assert!(fails(&r).contains("sending is locked"));
    assert!(fails(&tool(&mut s, &t, "mail_send", json!({"to": "z@example.com", "subject": "s", "body": "b"}))).contains("sending is locked"));
    s.mcp.owner_ui = true;
    s.run("mcp.unlockApproval", &json!({})).unwrap();
    assert!(!s.mcp.settings.approval.locked);
    let r = tool_at(&mut s, &t, "mail_send", json!({"pending_id": pending, "approval_code": code_for(now)}), now + 7);
    assert_eq!(answered(&r)["outcome"], json!("sent"), "{r}");
}

#[test]
fn a_code_before_the_secret_has_been_read_from_the_keychain_is_refused() {
    let Fixture { mut s, t, .. } = fixture("mcp-send-unread-secret");
    activate(&mut s);
    let held = tool(&mut s, &t, "mail_send", json!({"to": "a@example.com", "subject": "s", "body": "b"}));
    let pending = answered(&held)["pendingId"].as_str().unwrap().to_string();
    s.mcp.secret = None;
    assert!(s.mcp_wants_secret());
    let now = tick();
    let r = tool_at(&mut s, &t, "mail_send", json!({"pending_id": pending, "approval_code": code_for(now)}), now);
    assert!(fails(&r).contains("approval isn't ready"), "{r}");
    assert_eq!(s.mcp.settings.approval.failures, 0);
    // The keychain answers.
    s.apply(Event::AppSecretRead { what: "mcp-approval".into(), secret: Some(mailcraft_model::job::Secret(approval::base32(SECRET))) });
    assert!(!s.mcp_wants_secret());
    let r = tool_at(&mut s, &t, "mail_send", json!({"pending_id": pending, "approval_code": code_for(now)}), now);
    assert_eq!(answered(&r)["outcome"], json!("sent"), "{r}");
}

#[test]
fn the_owner_can_send_or_discard_a_held_message_at_the_desktop() {
    let Fixture { mut s, t, .. } = fixture("mcp-send-owner");
    activate(&mut s);
    s.mcp.owner_ui = true;
    let held = tool(&mut s, &t, "mail_send", json!({"to": "a@example.com", "subject": "From the desk", "body": "b"}));
    let pending = answered(&held)["pendingId"].as_str().unwrap().to_string();
    assert_eq!(s.run("mcp.discardPending", &json!({"id": "nothing"})).unwrap(), json!({"discarded": false}));
    assert!(matches!(s.run("mcp.sendPending", &json!({"id": "nothing"})), Err(CmdError::Params(_))));
    let sent = s.run("mcp.sendPending", &json!({"id": pending})).unwrap();
    assert!(sent["sent"].is_number(), "{sent}");
    assert!(s.mcp.pending.is_empty());
    assert!(s.folder_state("sample", "Sent").messages.iter().any(|m| m.subject == "From the desk"));
    // Discarded, it can't be released by a code.
    let held = tool(&mut s, &t, "mail_send", json!({"to": "b@example.com", "subject": "Dropped", "body": "b"}));
    let pending = answered(&held)["pendingId"].as_str().unwrap().to_string();
    assert_eq!(s.run("mcp.discardPending", &json!({"id": pending})).unwrap(), json!({"discarded": true}));
    let now = tick();
    let r = tool_at(&mut s, &t, "mail_send", json!({"pending_id": pending, "approval_code": code_for(now)}), now);
    assert!(fails(&r).contains("no such pending send"));
}

#[test]
fn the_day_changing_starts_the_count_again() {
    let Fixture { mut s, t, thread, .. } = fixture("mcp-send-day");
    let today = mailcraft_model::dates::today(&s.clock);
    s.mcp.settings.sent_today = (today - 1, 20);
    let r = tool(&mut s, &t, "mail_send", json!({"in_reply_to": thread.to_id(), "body": "ok"}));
    assert_eq!(answered(&r)["outcome"], json!("sent"), "{r}");
    assert_eq!(s.mcp.settings.sent_today, (today, 1));
}

#[test]
fn the_message_the_user_is_writing_is_left_alone_by_a_send() {
    let Fixture { mut s, t, thread, .. } = fixture("mcp-send-user-compose");
    s.run("mail.new", &json!({})).unwrap();
    s.run("compose.update", &json!({"to": "friend@example.com", "subject": "My own draft", "body": "Half a thought"})).unwrap();
    let mine = s.compose.clone().unwrap();
    let windows = s.composes.len();
    let r = tool(&mut s, &t, "mail_send", json!({"in_reply_to": thread.to_id(), "body": "ok"}));
    assert_eq!(answered(&r)["outcome"], json!("sent"), "{r}");
    assert_eq!(s.compose.as_ref().map(|c| (&c.to, &c.subject, &c.body, c.id)), Some((&mine.to, &mine.subject, &mine.body, mine.id)));
    assert_eq!(s.composes.len(), windows);
}

/// A server account, the user writing a message, and an approved message about to go out to it.
fn server_send(name: &str) -> (Session, String, String, i64, String) {
    let (mut s, id) =
        server_account(name, json!({"email": "zoe@example.org", "password": "pw"}), &[("INBOX", FolderRole::Inbox), ("Sent", FolderRole::Sent)]);
    s.take_jobs();
    let t = token();
    s.mcp_seed_token(&t, &ALL);
    activate(&mut s);
    let held = tool(&mut s, &t, "mail_send", json!({"to": "a@example.com", "subject": "Over the network", "body": "b", "from": id}));
    let pending = answered(&held)["pendingId"].as_str().unwrap().to_string();
    (s, t, id, tick(), pending)
}

#[test]
fn a_send_over_the_network_waits_for_the_servers_word() {
    let (mut s, t, id, now, pending) = server_send("mcp-send-server");
    s.run("mail.new", &json!({})).unwrap();
    s.run("compose.update", &json!({"subject": "Still mine"})).unwrap();
    let before = (s.compose.clone().map(|c| (c.id, c.subject, c.sending)), s.composes.len());
    let ask = request(1, "tools/call", json!({"name": "mail_send", "arguments": {"pending_id": pending, "approval_code": code_for(now)}}));
    let McpOutcome::Later(ticket) = s.mcp_handle(&t, &ask, now) else { panic!("it should wait for the server") };
    let sent_id = s.take_jobs().into_iter().find_map(|j| match j {
        Job::Send { id: sent, account, .. } if account == id => Some(sent),
        _ => None,
    });
    let sent_id = sent_id.expect("a send job");
    s.apply(Event::Sent { account: id.clone(), id: sent_id });
    let done = s.mcp_finished(now + 1);
    assert_eq!(done[0].0, ticket);
    let line: Value = serde_json::from_str(&done[0].1).unwrap();
    assert_eq!(answered(&line["result"]), &json!({"outcome": "sent", "id": sent_id}));
    assert_eq!((s.compose.clone().map(|c| (c.id, c.subject, c.sending)), s.composes.len()), before);
}

#[test]
fn a_send_that_fails_is_a_tool_error_and_opens_nothing_over_the_users_work() {
    let (mut s, t, id, now, pending) = server_send("mcp-send-failed");
    s.run("mail.new", &json!({})).unwrap();
    s.run("compose.update", &json!({"subject": "Still mine"})).unwrap();
    let mine = s.compose.clone().map(|c| (c.id, c.subject, c.sending, c.error));
    let ask = request(1, "tools/call", json!({"name": "mail_send", "arguments": {"pending_id": pending, "approval_code": code_for(now)}}));
    assert!(matches!(s.mcp_handle(&t, &ask, now), McpOutcome::Later(_)));
    let sent_id = s.take_jobs().into_iter().find_map(|j| if let Job::Send { id, .. } = j { Some(id) } else { None }).unwrap();
    s.apply(Event::SendFailed { account: id, id: sent_id, error: "the server said no".into() });
    let line: Value = serde_json::from_str(&s.mcp_finished(now + 1)[0].1).unwrap();
    assert!(fails(&line["result"]).contains("the server said no"));
    assert_eq!(s.compose.clone().map(|c| (c.id, c.subject, c.sending, c.error)), mine);
    assert!(s.composes.is_empty());
    assert!(s.notices.iter().any(|n| n.error && n.text.contains("Not sent")));
}

#[test]
fn a_send_queued_or_without_word_says_so() {
    let (mut s, t, id, now, pending) = server_send("mcp-send-queued");
    let ask = request(1, "tools/call", json!({"name": "mail_send", "arguments": {"pending_id": pending, "approval_code": code_for(now)}}));
    assert!(matches!(s.mcp_handle(&t, &ask, now), McpOutcome::Later(_)));
    let sent_id = s.take_jobs().into_iter().find_map(|j| if let Job::Send { id, .. } = j { Some(id) } else { None }).unwrap();
    s.apply(Event::Queued { account: id, id: sent_id, error: "offline".into(), kind: Default::default() });
    let line: Value = serde_json::from_str(&s.mcp_finished(now + 1)[0].1).unwrap();
    assert_eq!(answered(&line["result"])["outcome"], json!("queued"));
    assert_eq!(answered(&line["result"])["error"], json!("offline"));
    // No word at all: handed to the server, after 60 s.
    let account = s.accounts[0].config.id.clone();
    let held = tool(&mut s, &t, "mail_send", json!({"to": "a@example.com", "subject": "Silent", "body": "b", "from": account}));
    let pending = answered(&held)["pendingId"].as_str().unwrap().to_string();
    let later = now + 120_000;
    let ask = request(2, "tools/call", json!({"name": "mail_send", "arguments": {"pending_id": pending, "approval_code": code_for(later)}}));
    assert!(matches!(s.mcp_handle(&t, &ask, later), McpOutcome::Later(_)));
    assert!(s.mcp_finished(later + 59_999).is_empty());
    let line: Value = serde_json::from_str(&s.mcp_finished(later + 60_000)[0].1).unwrap();
    assert_eq!(answered(&line["result"])["outcome"], json!("sending"));
}

#[test]
fn nothing_secret_reaches_the_log_a_notice_or_a_debug_print() {
    let Fixture { mut s, t, thread, .. } = fixture("mcp-send-redacted");
    let _ = mcp_logged();
    s.mcp.owner_ui = true;
    s.mcp.random = Some(|n| Some(vec![0x21; n]));
    s.run("mcp.setupApproval", &json!({})).unwrap();
    let secret = s.mcp.setup.clone().unwrap().secret_b32;
    let now = tick();
    let code = code_for_secret(&s.mcp.setup.clone().unwrap().bytes, NOW * 1000);
    s.run("mcp.confirmApproval", &json!({"code": code})).unwrap();
    let held = tool(&mut s, &t, "mail_send", json!({"to": "a@example.com", "subject": "s", "body": "b"}));
    let pending = answered(&held)["pendingId"].as_str().unwrap().to_string();
    let _ = tool_at(&mut s, &t, "mail_send", json!({"pending_id": pending, "approval_code": "000000"}), now + 1);
    let _ = tool(&mut s, &t, "mail_send", json!({"in_reply_to": thread.to_id(), "body": "ok"}));
    let mut seen = mcp_logged();
    seen.extend(s.notices.iter().map(|n| n.text.clone()));
    seen.push(format!("{:?}", s.mcp));
    seen.push(format!("{:?}", s.mcp.setup));
    seen.push(format!("{:?}", s.mcp.pending));
    seen.push(format!("{:?}", s.mcp.revealed));
    seen.push(format!("{:?}", s.take_jobs().iter().filter(|j| matches!(j, Job::KeepAppSecret { .. })).collect::<Vec<_>>()));
    for text in &seen {
        assert!(!text.contains(&secret) && !text.contains(&t) && !text.contains("000000") && !text.contains(&code), "{text}");
    }
    assert!(seen.iter().any(|l| l.contains("approval code was wrong")));
}

fn code_for_secret(bytes: &[u8], now_ms: i64) -> String {
    format!("{:06}", approval::code_at(bytes, approval::step(now_ms.div_euclid(1000))).unwrap())
}
