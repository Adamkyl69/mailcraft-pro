//! Setting approval up, resetting it, and the rules the owner sets, through the commands that only
//! the owner's click runs.

use super::*;
use crate::mcp::approval::{self, ApprovalState};
use mailcraft_model::Job;
use mailcraft_model::job::Secret;

fn owner(name: &str) -> Session {
    let mut s = sample_session(name);
    s.mcp.owner_ui = true;
    s.mcp.random = Some(|n| Some((0..n).map(|i| u8::try_from(i * 7 % 251).unwrap_or(0)).collect()));
    s
}

fn code(bytes: &[u8], now: i64) -> String {
    format!("{:06}", approval::code_at(bytes, approval::step(now)).unwrap())
}

#[test]
fn approval_is_set_up_with_a_code_from_the_authenticator() {
    let mut s = owner("mcp-approval-approval-is-set-up-with-a-code");
    assert!(matches!(s.run("mcp.confirmApproval", &json!({"code": "123456"})), Err(CmdError::Disabled(_))));
    s.run("mcp.setupApproval", &json!({})).unwrap();
    let setup = s.mcp.setup.clone().unwrap();
    assert_eq!(setup.uri, format!("otpauth://totp/MailCraft?secret={}&issuer=MailCraft", setup.secret_b32));
    assert_eq!(approval::unbase32(&setup.secret_b32), Some(setup.bytes.clone()));
    assert_eq!(setup.bytes.len(), 20);
    // A wrong code, and a malformed one, leave it waiting.
    let wrong = if code(&setup.bytes, NOW) == "000000" { "000001" } else { "000000" };
    assert!(matches!(s.run("mcp.confirmApproval", &json!({"code": wrong})), Err(CmdError::Params(m)) if m.contains("try the next one")));
    assert!(matches!(s.run("mcp.confirmApproval", &json!({"code": "12"})), Err(CmdError::Params(_))));
    assert!(s.mcp.setup.is_some() && s.mcp.settings.approval.state == ApprovalState::Off);
    // The right one: on, kept in the keychain, never in mcp.json.
    assert_eq!(s.run("mcp.confirmApproval", &json!({"code": code(&setup.bytes, NOW)})).unwrap(), json!({"active": true}));
    assert!(s.mcp.setup.is_none() && s.mcp.secret.as_deref() == Some(&setup.bytes[..]));
    let approval = &s.mcp.settings.approval;
    assert_eq!((approval.state, approval.last_step, approval.locked, approval.failures), (ApprovalState::Active, approval::step(NOW), false, 0));
    let jobs = s.take_jobs();
    assert!(
        jobs.iter()
            .any(|j| matches!(j, Job::KeepAppSecret { what, secret: Some(Secret(b32)) } if what == "mcp-approval" && *b32 == setup.secret_b32))
    );
    let kept = std::fs::read_to_string(s.store.as_ref().unwrap().root().join("mcp.json")).unwrap();
    assert!(!kept.contains(&setup.secret_b32) && kept.contains("\"state\":\"active\""), "{kept}");
    // The code that confirmed it can't approve a send.
    let mut rec = s.mcp.settings.approval.clone();
    assert_eq!(approval::check(&mut rec, &setup.bytes, code(&setup.bytes, NOW).parse().unwrap(), NOW), approval::Verdict::Replayed);
    // Set up again only after a reset.
    assert!(matches!(s.run("mcp.setupApproval", &json!({})), Err(CmdError::Params(_))));
}

#[test]
fn approval_needs_a_secure_random_source() {
    let mut s = owner("mcp-approval-approval-needs-a-secure-random");
    s.mcp.random = None;
    assert!(matches!(s.run("mcp.setupApproval", &json!({})), Err(CmdError::Disabled(m)) if m == "no secure random source"));
    s.mcp.random = Some(|_| Some(vec![1, 2]));
    assert!(s.run("mcp.setupApproval", &json!({})).is_err());
    assert!(s.mcp.setup.is_none());
}

#[test]
fn a_keychain_that_cant_keep_the_secret_turns_approval_off_again() {
    let mut s = owner("mcp-approval-a-keychain-that-cant-keep-the-");
    s.run("mcp.setupApproval", &json!({})).unwrap();
    let bytes = s.mcp.setup.clone().unwrap().bytes;
    s.run("mcp.confirmApproval", &json!({"code": code(&bytes, NOW)})).unwrap();
    s.apply(Event::AppSecretKept { what: "mcp-approval".into(), error: Some("the keychain is locked".into()) });
    assert_eq!(s.mcp.settings.approval.state, ApprovalState::Off);
    assert!(s.mcp.secret.is_none() && !s.mcp_wants_secret());
    assert!(s.notices.iter().any(|n| n.error && n.text == "Approval couldn't be kept in the keychain: the keychain is locked"));
    // A secret kept without trouble, or another's, changes nothing.
    s.run("mcp.setupApproval", &json!({})).unwrap();
    let bytes = s.mcp.setup.clone().unwrap().bytes;
    s.run("mcp.confirmApproval", &json!({"code": code(&bytes, NOW)})).unwrap();
    s.apply(Event::AppSecretKept { what: "mcp-approval".into(), error: None });
    s.apply(Event::AppSecretKept { what: "something-else".into(), error: Some("x".into()) });
    assert_eq!(s.mcp.settings.approval.state, ApprovalState::Active);
}

#[test]
fn a_restart_asks_the_keychain_for_the_secret_and_without_it_approval_has_to_be_set_up_again() {
    let dir = scratch("mcp-restart");
    std::fs::create_dir_all(&dir).unwrap();
    let doc = json!({"version": 1, "enabled": true, "tokens": [], "approval": {"state": "active", "lastStep": 5, "failures": 2, "locked": false}});
    std::fs::write(dir.join("mcp.json"), doc.to_string()).unwrap();
    let mut s = Session::open(Some(Store::open(dir).unwrap()), mailcraft_model::dates::Clock::new(NOW, 0));
    assert_eq!(s.mcp.settings.approval.last_step, 5);
    assert!(s.mcp_wants_secret());
    // The keychain has none: still wanted, so the card says "Set up again".
    s.apply(Event::AppSecretRead { what: "mcp-approval".into(), secret: None });
    assert!(s.mcp_wants_secret());
    s.mcp.owner_ui = true;
    s.mcp.random = Some(|n| Some(vec![4; n]));
    s.run("mcp.setupApproval", &json!({})).unwrap();
    // Another secret's answer isn't ours, and one that isn't base32 isn't a secret.
    s.apply(Event::AppSecretRead { what: "other".into(), secret: Some(Secret("MZXW6YTB".into())) });
    s.apply(Event::AppSecretRead { what: "mcp-approval".into(), secret: Some(Secret("not base32!".into())) });
    assert!(s.mcp_wants_secret());
    s.apply(Event::AppSecretRead { what: "mcp-approval".into(), secret: Some(Secret("MZXW6YTB".into())) });
    assert!(!s.mcp_wants_secret() && s.mcp.secret.as_deref() == Some(&b"fooba"[..]));
}

#[test]
fn a_reset_forgets_the_secret_and_discards_what_was_held() {
    let mut s = owner("mcp-approval-a-reset-forgets-the-secret-and");
    s.run("mcp.setupApproval", &json!({})).unwrap();
    let bytes = s.mcp.setup.clone().unwrap().bytes;
    s.run("mcp.confirmApproval", &json!({"code": code(&bytes, NOW)})).unwrap();
    s.take_jobs();
    let t = token();
    s.mcp_seed_token(&t, &ALL);
    let held = tool(&mut s, &t, "mail_send", json!({"to": "a@example.com", "subject": "s", "body": "b"}));
    assert_eq!(ok(&held)["outcome"], json!("needsCode"));
    s.run("mcp.resetApproval", &json!({})).unwrap();
    assert!(s.mcp.pending.is_empty() && s.mcp.secret.is_none() && s.mcp.setup.is_none());
    assert_eq!(s.mcp.settings.approval, Default::default());
    assert!(s.take_jobs().iter().any(|j| matches!(j, Job::KeepAppSecret { what, secret: None } if what == "mcp-approval")));
    // The old secret's codes get nowhere: approval is off, so a message needing one is refused.
    let r = tool(&mut s, &t, "mail_send", json!({"to": "a@example.com", "subject": "s", "body": "b"}));
    assert!(fails(&r).contains("approval isn't set up"));
}

#[test]
fn the_rules_are_set_whole_or_not_at_all() {
    let mut s = owner("mcp-approval-the-rules-are-set-whole-or-not");
    assert_eq!(s.run("mcp.setRules", &json!({})).unwrap(), json!({"noCodeReplies": true, "dailyLimit": 20}));
    assert_eq!(
        s.run("mcp.setRules", &json!({"noCodeReplies": false, "dailyLimit": 200})).unwrap(),
        json!({"noCodeReplies": false, "dailyLimit": 200})
    );
    assert_eq!(s.run("mcp.setRules", &json!({"dailyLimit": 0})).unwrap(), json!({"noCodeReplies": false, "dailyLimit": 0}));
    let before = s.mcp.settings.rules.clone();
    for bad in [
        json!({"dailyLimit": 201}),
        json!({"dailyLimit": -1}),
        json!({"dailyLimit": "x"}),
        json!({"dailyLimit": 1.5}),
        json!({"noCodeReplies": "yes"}),
        json!({"noCodeReplies": true, "dailyLimit": 999}),
    ] {
        assert!(matches!(s.run("mcp.setRules", &bad), Err(CmdError::Params(_))), "{bad}");
    }
    assert_eq!(s.mcp.settings.rules, before);
}

#[test]
fn the_commands_for_approval_only_run_from_the_owners_click_and_survive_junk() {
    let mut s = sample_session("mcp-approval-gated");
    s.mcp.random = Some(|n| Some(vec![1; n]));
    for (id, params) in [
        ("mcp.setupApproval", json!({})),
        ("mcp.confirmApproval", json!({"code": "123456"})),
        ("mcp.resetApproval", json!({})),
        ("mcp.unlockApproval", json!({})),
        ("mcp.setRules", json!({"dailyLimit": 5})),
        ("mcp.sendPending", json!({"id": "x"})),
        ("mcp.discardPending", json!({"id": "x"})),
    ] {
        assert!(matches!(s.run(id, &params), Err(CmdError::Disabled(m)) if m.contains("Connected apps")), "{id}");
    }
    assert_eq!(s.mcp.settings.rules.daily_limit, 20);
    s.mcp.owner_ui = true;
    let t = token();
    s.mcp_seed_token(&t, &ALL);
    for id in [
        "mcp.setupApproval",
        "mcp.confirmApproval",
        "mcp.resetApproval",
        "mcp.unlockApproval",
        "mcp.setRules",
        "mcp.sendPending",
        "mcp.discardPending",
    ] {
        for junk in
            [json!(null), json!({}), json!({"code": 5, "id": [], "dailyLimit": {}, "noCodeReplies": null}), json!({"code": "\u{0}", "id": "\u{0}"})]
        {
            let _ = s.run(id, &junk);
        }
    }
    // None of them can be run through mail_run.
    let key = inbox(&mut s)[0].to_id();
    for id in ["mcp.resetApproval", "mcp.setRules", "mcp.sendPending"] {
        let r = tool(&mut s, &t, "mail_run", json!({"command": id, "keys": [key.clone()]}));
        assert!(fails(&r).contains("isn't one an AI app may run"), "{id}");
    }
}
