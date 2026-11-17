//! Edge cases of `docs/specs/mcp-server-build.md` that the other files don't show on their own:
//! each test names its number. What needs a settings file really kept (the lock, the wrong codes
//! and the used steps across a restart) runs over a cache on disk.

use super::sending::answered;
use super::*;
use crate::mcp::approval::{self, ApprovalState};
use crate::mcp::protocol;
use mailcraft_model::job::Secret;

/// `{"a":{"a":...}}` with `n` objects.
fn nested(n: usize) -> Value {
    (1..n).fold(json!(1), |v, _| json!({"a": v}))
}

fn nested_call(depth: usize) -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "mail_accounts", "arguments": nested(depth)}})
}

#[test]
fn edge_case_7_nesting_64_deep_is_accepted_and_then_a_tool_argument_error_and_65_is_refused() {
    let (mut s, t) = mcp_session("mcp-edge7", &ALL);
    // The request is two levels down before the arguments' own nesting starts.
    let at_limit = reply(s.mcp_handle(&t, &nested_call(crate::mcp::MAX_DEPTH - 2), tick()));
    assert_eq!(at_limit["result"]["isError"], json!(true), "{at_limit}");
    assert!(at_limit["result"]["content"][0]["text"].as_str().unwrap().contains("unknown argument"), "{at_limit}");
    let over = reply(s.mcp_handle(&t, &nested_call(crate::mcp::MAX_DEPTH - 1), tick()));
    assert_eq!((over["error"]["code"].clone(), over["id"].clone()), (json!(protocol::INVALID_REQUEST), json!(1)), "{over}");
    // Far deeper than a line of JSON can be written: still an answer.
    let silly = reply(s.mcp_handle(&t, &nested_call(1_000), tick()));
    assert_eq!(silly["error"]["code"], json!(protocol::INVALID_REQUEST));
}

#[test]
fn edge_case_12_a_token_of_the_wrong_shape_is_refused_like_an_unknown_one() {
    let (mut s, t) = mcp_session("mcp-edge12", &ALL);
    for bad in ["mcp_", "secret", "mcp_short", &t[..t.len() - 1], &format!("{t}0")] {
        let r = call(&mut s, bad, "tools/list", json!({}));
        assert_eq!(r["error"]["code"], json!(protocol::TOKEN_REFUSED), "{bad}");
    }
}

#[test]
fn edge_case_15_a_settings_file_that_cant_be_read_leaves_mcp_off_unchanged_and_says_so() {
    let store = temp("mcp-edge15");
    sample::seed_if_empty(&store, NOW).unwrap();
    // A file that can't be read at all (the store sets a damaged one aside instead): a folder in its place.
    let path = store.root().join("mcp.json");
    std::fs::create_dir(&path).unwrap();
    let mut s = Session::open(Some(store.clone()), mailcraft_model::dates::Clock::new(NOW, 0));
    assert!(s.mcp_unusable().is_some());
    assert!(s.notices.iter().any(|n| n.error && n.text.contains("mcp.json") && n.text.contains("couldn't be read")), "a notice says so");
    // Off this run, whatever token is offered.
    assert!(s.mcp.settings.tokens.is_empty() && !s.mcp.settings.enabled);
    assert_eq!(call(&mut s, &token(), "ping", json!({}))["error"]["code"], json!(protocol::TOKEN_REFUSED));
    // No token can be made, nothing switched on, approval not set up, until it's fixed.
    s.mcp.owner_ui = true;
    s.mcp.random = Some(|n| Some(vec![3; n]));
    for (id, params) in [
        ("mcp.createToken", json!({"name": "A"})),
        ("mcp.setEnabled", json!({"on": true})),
        ("mcp.revokeToken", json!({"id": "x"})),
        ("mcp.setupApproval", json!({})),
        ("mcp.confirmApproval", json!({"code": "123456"})),
        ("mcp.resetApproval", json!({})),
        ("mcp.unlockApproval", json!({})),
        ("mcp.setRules", json!({"dailyLimit": 5})),
    ] {
        let r = s.run(id, &params);
        assert!(matches!(&r, Err(CmdError::Disabled(why)) if why.contains("couldn't be read")), "{id}: {r:?}");
    }
    assert!(s.mcp.settings.tokens.is_empty() && s.mcp.revealed.is_none() && s.mcp.setup.is_none());
    // Nothing was written over it, and nothing set aside as damaged: it's still what it was.
    assert!(path.is_dir(), "the file wasn't touched");
    assert!(!store.root().join("mcp.json.bad").exists());
    // A calls' use of a token isn't saved either (the "last used" of a token that can't exist).
    drop(s);
    let s = Session::open(Some(store), mailcraft_model::dates::Clock::new(NOW, 0));
    assert!(s.mcp_unusable().is_some(), "still unread the next run, until the owner fixes the file");
}

// ---- a settings file kept on disk ----

/// A session on the sample over a cache kept in a folder of its own, with a token that may send
/// (the token as it was shown), MCP on, and approval set up (its secret as the authenticator has it).
struct Kept {
    store: Store,
    token: String,
    secret_b32: String,
    s: Session,
}

fn kept(name: &str) -> Kept {
    let store = temp(name);
    assert!(sample::seed_if_empty(&store, NOW).unwrap());
    let mut s = Session::open(Some(store.clone()), mailcraft_model::dates::Clock::new(NOW, 0));
    s.mcp.owner_ui = true;
    s.mcp.random = Some(|n| Some((0..n).map(|i| u8::try_from(i * 3 % 251).unwrap_or(1)).collect()));
    s.run("mcp.createToken", &json!({"name": "A", "permissions": ["read", "send"]})).unwrap();
    let token = s.mcp.revealed.clone().unwrap().token;
    s.run("mcp.setEnabled", &json!({"on": true})).unwrap();
    s.run("mcp.setupApproval", &json!({})).unwrap();
    let setup = s.mcp.setup.clone().unwrap();
    let code = format!("{:06}", approval::code_at(&setup.bytes, approval::step(NOW)).unwrap());
    s.run("mcp.confirmApproval", &json!({"code": code})).unwrap();
    s.take_jobs();
    Kept { store, token, secret_b32: setup.secret_b32, s }
}

impl Kept {
    /// The app started again over the same files, and the keychain giving the secret back.
    fn restart(self) -> Kept {
        let Kept { store, token, secret_b32, s } = self;
        drop(s);
        let mut s = Session::open(Some(store.clone()), mailcraft_model::dates::Clock::new(NOW, 0));
        s.mcp.owner_ui = true;
        s.mcp.random = Some(|n| Some(vec![0x33; n]));
        assert!(s.mcp_wants_secret(), "approval is on and its secret isn't here until the keychain gives it");
        s.apply(Event::AppSecretRead { what: "mcp-approval".into(), secret: Some(Secret(secret_b32.clone())) });
        assert!(!s.mcp_wants_secret());
        Kept { store, token, secret_b32, s }
    }

    /// A message held for a code, and its id.
    fn hold(&mut self, subject: &str, now_ms: i64) -> String {
        let r = tool_at(&mut self.s, &self.token, "mail_send", json!({"to": "a@example.com", "subject": subject, "body": "b"}), now_ms);
        answered(&r)["pendingId"].as_str().unwrap().to_string()
    }

    fn approve(&mut self, pending: &str, code: &str, now_ms: i64) -> Value {
        tool_at(&mut self.s, &self.token, "mail_send", json!({"pending_id": pending, "approval_code": code}), now_ms)
    }
}

/// A code for this run's secret: not `SECRET`, the one the card set up.
/// An hour on, a second into a 30-second step: a few seconds more stay in it.
fn step_start_ms() -> i64 {
    ((NOW + 3600) / 30 * 30 + 1) * 1000
}

fn code_of(k: &Kept, now_ms: i64) -> String {
    let bytes = approval::unbase32(&k.secret_b32).unwrap();
    format!("{:06}", approval::code_at(&bytes, approval::step(now_ms / 1000)).unwrap())
}

#[test]
fn edge_case_51_five_wrong_codes_lock_and_the_lock_and_the_count_before_it_are_saved() {
    let mut k = kept("mcp-edge51");
    let later = step_start_ms();
    let pending = k.hold("First", later);
    // Three wrong ones, then the app is closed.
    for n in 0..3 {
        let r = k.approve(&pending, "000000", later + n);
        assert_eq!(fails(&r), "that code isn't right", "{n}");
    }
    let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(k.store.root().join("mcp.json")).unwrap()).unwrap();
    assert_eq!(on_disk["approval"]["failures"], json!(3), "{on_disk}");
    let mut k = k.restart();
    assert_eq!(k.s.mcp.settings.approval.failures, 3, "wrong codes aren't forgiven by closing the app");
    // Held messages are in memory only: a new one, and two more wrong codes are the fifth.
    assert!(k.s.mcp.pending.is_empty());
    let pending = k.hold("Second", later + 5);
    assert!(fails(&k.approve(&pending, "000000", later + 10)).contains("that code isn't right"));
    let fifth = k.approve(&pending, "000000", later + 11);
    assert!(fails(&fifth).contains("now locked"), "{fifth}");
    assert!(k.s.mcp.settings.approval.locked);
    // Locked, the right code gets nowhere, nor does a new message, and the lock survives another restart.
    let right = code_of(&k, later + 20_000);
    assert!(fails(&k.approve(&pending, &right, later + 20_000)).contains("sending is locked"));
    let mut k = k.restart();
    assert!(k.s.mcp.settings.approval.locked, "the lock is saved");
    let r = tool_at(&mut k.s, &k.token, "mail_send", json!({"to": "a@example.com", "subject": "Third", "body": "b"}), later + 30_000);
    assert!(fails(&r).contains("sending is locked"), "{r}");
    // Only the owner's unlocking opens it again, and that is saved too.
    k.s.run("mcp.unlockApproval", &json!({})).unwrap();
    let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(k.store.root().join("mcp.json")).unwrap()).unwrap();
    assert_eq!((on_disk["approval"]["locked"].clone(), on_disk["approval"]["failures"].clone()), (json!(false), json!(0)), "{on_disk}");
    let pending = k.hold("Fourth", later + 80_000);
    let now = later + 90_000;
    assert_eq!(answered(&k.approve(&pending, &code_of(&k, now), now))["outcome"], json!("sent"));
}

#[test]
fn edge_case_50_a_code_that_was_used_is_refused_again_after_the_app_restarts() {
    let mut k = kept("mcp-edge50");
    let now = step_start_ms();
    let pending = k.hold("One", now);
    let code = code_of(&k, now);
    assert_eq!(answered(&k.approve(&pending, &code, now))["outcome"], json!("sent"));
    let mut k = k.restart();
    // Held again; the same code, the same moment (and a step either side that is no later).
    let pending = k.hold("Two", now + 1);
    for at in [now, now + 5_000, now - 30_000] {
        let r = k.approve(&pending, &code_of(&k, at), at);
        assert_eq!(fails(&r), "that code isn't right", "replayed at {at}: {r}");
    }
    // The next step's code is a new one.
    let next = now + 30_000;
    assert_eq!(answered(&k.approve(&pending, &code_of(&k, next), next))["outcome"], json!("sent"));
}

#[test]
fn edge_case_52_a_reset_forgets_the_old_secret_for_good_and_a_new_one_has_its_own_codes() {
    let mut k = kept("mcp-edge52");
    let now = step_start_ms();
    let old_b32 = k.secret_b32.clone();
    let pending = k.hold("Held", now);
    k.s.run("mcp.resetApproval", &json!({})).unwrap();
    assert!(k.s.mcp.pending.is_empty());
    let jobs = k.s.take_jobs();
    assert!(jobs.iter().any(|j| matches!(j, Job::KeepAppSecret { what, secret: None } if what == "mcp-approval")));
    // Set up again with another secret: the old one's codes mean nothing.
    k.s.mcp.random = Some(|n| Some(vec![0x77; n]));
    k.s.run("mcp.setupApproval", &json!({})).unwrap();
    let new = k.s.mcp.setup.clone().unwrap();
    assert_ne!(new.secret_b32, old_b32);
    let code = format!("{:06}", approval::code_at(&new.bytes, approval::step(NOW + 3600)).unwrap());
    k.s.clock = mailcraft_model::dates::Clock::new(NOW + 3600, 0);
    k.s.run("mcp.confirmApproval", &json!({"code": code})).unwrap();
    let held = tool_at(&mut k.s, &k.token, "mail_send", json!({"to": "a@example.com", "subject": "New", "body": "b"}), now + 60_000);
    let pending2 = answered(&held)["pendingId"].as_str().unwrap().to_string();
    assert_ne!(pending, pending2);
    let old_bytes = approval::unbase32(&old_b32).unwrap();
    let old_code = format!("{:06}", approval::code_at(&old_bytes, approval::step(now / 1000 + 90)).unwrap());
    let r = k.approve(&pending2, &old_code, now + 90_000);
    assert_eq!(fails(&r), "that code isn't right", "the old secret's code: {r}");
}

#[test]
fn edge_case_16_a_future_version_with_an_approval_in_it_keeps_the_approval() {
    let store = temp("mcp-edge16");
    let doc = json!({"version": 7, "enabled": true, "tokens": [], "approval": {"state": "active", "lastStep": 12, "failures": 4, "locked": false, "futureField": 1}, "alsoNew": {}});
    std::fs::write(store.root().join("mcp.json"), doc.to_string()).unwrap();
    let s = Session::open(Some(store), mailcraft_model::dates::Clock::new(NOW, 0));
    let a = &s.mcp.settings.approval;
    assert_eq!((a.state, a.last_step, a.failures, a.locked), (ApprovalState::Active, 12, 4, false));
}
