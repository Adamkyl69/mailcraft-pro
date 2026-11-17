//! Who may do what: the token's permissions, and the commands only the owner's click may run.

use super::*;

fn owner(s: &mut Session) {
    s.mcp.owner_ui = true;
    s.mcp.random = Some(|n| Some((0..n).map(|i| u8::try_from(i % 251).unwrap_or(0)).collect()));
}

#[test]
fn a_token_may_use_only_what_its_permissions_allow() {
    let (mut s, t) = mcp_session("mcp-perms", &[Permission::Read]);
    let listed = call(&mut s, &t, "tools/list", json!({}));
    let names: Vec<&str> = listed["result"]["tools"].as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["mail_accounts", "mail_list", "mail_search", "mail_read", "mail_commands", "monitor_log"]);
    // Asking for one that isn't listed is refused as a tool error, and nothing runs.
    let key = inbox(&mut s)[0].to_id();
    let before = seen(&s);
    let r = tool(&mut s, &t, "mail_run", json!({"command": "mail.markRead", "keys": [key]}));
    assert_eq!(fails(&r), "this token may not use mail_run");
    assert_eq!(fails(&tool(&mut s, &t, "mail_sync", json!({}))), "this token may not use mail_sync");
    assert_eq!(seen(&s), before);
    // `mail_commands` lists nothing to run for it.
    assert_eq!(ok(&tool(&mut s, &t, "mail_commands", json!({})))["commands"], json!([]));
}

#[test]
fn a_token_that_may_organize_is_offered_the_commands_it_may_run() {
    let (mut s, t) = mcp_session("mcp-commands", &[Permission::Read, Permission::Organize]);
    let r = tool(&mut s, &t, "mail_commands", json!({}));
    let commands = ok(&r)["commands"].as_array().unwrap();
    let ids: Vec<&str> = commands.iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(ids, crate::mcp::tools::RUNNABLE);
    assert!(commands.iter().all(|c| c["label"].is_string() && c["params"].is_string()));
}

#[test]
fn the_commands_that_set_mcp_up_only_run_from_the_owners_click() {
    let mut s = sample_session("mcp-owner-only");
    s.mcp.random = Some(|n| Some(vec![5u8; n]));
    let before = s.mcp.settings.clone();
    for (id, params) in [
        ("mcp.setEnabled", json!({"on": true})),
        ("mcp.createToken", json!({"name": "x"})),
        ("mcp.dismissToken", json!({})),
        ("mcp.revokeToken", json!({"id": "x"})),
    ] {
        let r = s.run(id, &params);
        assert!(matches!(&r, Err(CmdError::Disabled(why)) if why.contains("Connected apps")), "{id}: {r:?}");
    }
    assert_eq!(s.mcp.settings, before);
    assert!(s.mcp.revealed.is_none());
    // Listed, as every command is, with where they live.
    let listed = s.registry().describe();
    assert!(listed.as_array().unwrap().iter().any(|c| c["id"] == "mcp.createToken" && c["location"] == "File › Options"));
}

#[test]
fn mail_run_and_the_junk_params_cant_reach_them_either() {
    let (mut s, t) = mcp_session("mcp-owner-tools", &ALL);
    let key = inbox(&mut s)[0].to_id();
    let r = tool(&mut s, &t, "mail_run", json!({"command": "mcp.createToken", "keys": [key]}));
    assert!(fails(&r).contains("isn't one an AI app may run"));
    assert!(s.mcp.revealed.is_none());
    // With the owner's click they take any params without panicking.
    owner(&mut s);
    for id in ["mcp.setEnabled", "mcp.createToken", "mcp.dismissToken", "mcp.revokeToken"] {
        for junk in [
            json!(null),
            json!({}),
            json!({"name": 5, "permissions": "read", "id": [], "on": "yes"}),
            json!({"name": "\u{0}\u{7}", "permissions": [1, null]}),
        ] {
            let _ = s.run(id, &junk);
        }
    }
}

#[test]
fn a_token_is_made_without_a_secure_random_source_never() {
    let mut s = sample_session("mcp-no-random");
    s.mcp.owner_ui = true;
    let r = s.run("mcp.createToken", &json!({"name": "A"}));
    assert!(matches!(&r, Err(CmdError::Disabled(why)) if why == "no secure random source"), "{r:?}");
    assert!(s.mcp.settings.tokens.is_empty());
    // A source that fails, or gives the wrong number of bytes, makes none either.
    s.mcp.random = Some(|_| None);
    assert!(s.run("mcp.createToken", &json!({"name": "A"})).is_err());
    s.mcp.random = Some(|_| Some(vec![1, 2, 3]));
    assert!(s.run("mcp.createToken", &json!({"name": "A"})).is_err());
    assert!(s.mcp.settings.tokens.is_empty());
}

#[test]
fn a_tokens_name_is_cleaned_and_limited() {
    let mut s = sample_session("mcp-names");
    owner(&mut s);
    for bad in [
        json!({}),
        json!({"name": ""}),
        json!({"name": "   "}),
        json!({"name": "\u{0}\u{1b}\n"}),
        json!({"name": "n".repeat(65)}),
        json!({"name": 5}),
    ] {
        assert!(matches!(s.run("mcp.createToken", &bad), Err(CmdError::Params(_))), "{bad}");
    }
    assert!(s.mcp.settings.tokens.is_empty());
    s.run("mcp.createToken", &json!({"name": "  Claude\u{0} Desktop \n"})).unwrap();
    assert_eq!(s.mcp.settings.tokens[0].name, "Claude Desktop");
    s.mcp.random = Some(|n| Some(vec![6u8; n]));
    s.run("mcp.createToken", &json!({"name": "n".repeat(64)})).unwrap();
    assert_eq!(s.mcp.settings.tokens[1].name.len(), 64);
}

#[test]
fn read_is_always_held_and_an_unknown_permission_is_refused() {
    let mut s = sample_session("mcp-permissions");
    owner(&mut s);
    for bad in [json!({"name": "A", "permissions": ["root"]}), json!({"name": "A", "permissions": "read"}), json!({"name": "A", "permissions": [5]})]
    {
        assert!(matches!(s.run("mcp.createToken", &bad), Err(CmdError::Params(_))), "{bad}");
    }
    s.run("mcp.createToken", &json!({"name": "A", "permissions": ["organize"]})).unwrap();
    assert_eq!(s.mcp.settings.tokens[0].permissions, [Permission::Read, Permission::Organize].into_iter().collect());
    s.mcp.random = Some(|n| Some(vec![2u8; n]));
    s.run("mcp.createToken", &json!({"name": "B"})).unwrap();
    assert_eq!(s.mcp.settings.tokens[1].permissions, [Permission::Read].into_iter().collect());
}

#[test]
fn the_token_is_revealed_once_and_never_in_an_answer() {
    let mut s = sample_session("mcp-reveal");
    owner(&mut s);
    let made = s.run("mcp.createToken", &json!({"name": "A"})).unwrap();
    let revealed = s.mcp.revealed.clone().unwrap();
    assert_eq!(revealed.name, "A");
    assert!(crate::mcp::tokens::looks_like_token(&revealed.token));
    assert!(!made.to_string().contains(&revealed.token));
    assert_eq!(made["id"], json!(&crate::mcp::tokens::hash_hex(&revealed.token)[..12]));
    // What's kept is the hash.
    let kept = serde_json::to_string(&s.mcp.settings).unwrap();
    assert!(!kept.contains(&revealed.token));
    s.run("mcp.dismissToken", &json!({})).unwrap();
    assert!(s.mcp.revealed.is_none());
}

#[test]
fn two_tokens_whose_ids_collide_are_drawn_again_up_to_three_times() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static DRAWS: AtomicUsize = AtomicUsize::new(0);
    let mut s = sample_session("mcp-collide");
    owner(&mut s);
    s.mcp.random = Some(|n| {
        let draw = DRAWS.fetch_add(1, Ordering::SeqCst);
        // The first token takes draw 0; the second draws the same, twice, then a new one.
        Some(vec![if draw <= 2 { 1u8 } else { 9u8 }; n])
    });
    s.run("mcp.createToken", &json!({"name": "A"})).unwrap();
    DRAWS.store(0, Ordering::SeqCst);
    s.mcp.random = Some(|n| Some(vec![if DRAWS.fetch_add(1, Ordering::SeqCst) < 2 { 1u8 } else { 9u8 }; n]));
    let second = s.run("mcp.createToken", &json!({"name": "B"})).unwrap();
    assert_eq!(s.mcp.settings.tokens.len(), 2);
    assert_ne!(second["id"], json!(s.mcp.settings.tokens[0].id));
    // Three in a row that collide: no token.
    s.mcp.random = Some(|n| Some(vec![1u8; n]));
    let r = s.run("mcp.createToken", &json!({"name": "C"}));
    assert!(matches!(r, Err(CmdError::Failed(_))), "{r:?}");
    assert_eq!(s.mcp.settings.tokens.len(), 2);
}

#[test]
fn at_most_twenty_tokens_exist() {
    let mut s = sample_session("mcp-twenty");
    owner(&mut s);
    s.mcp.random = Some(|n| Some(vec![NEXT.with(Cell::get); n]));
    for i in 0..20u8 {
        NEXT.with(|c| c.set(i));
        s.run("mcp.createToken", &json!({"name": format!("app {i}")})).unwrap();
    }
    assert_eq!(s.mcp.settings.tokens.len(), 20);
    s.mcp.random = Some(|n| Some(vec![250u8; n]));
    assert!(matches!(s.run("mcp.createToken", &json!({"name": "one too many"})), Err(CmdError::Params(_))));
}

thread_local!(static NEXT: Cell<u8> = const { Cell::new(0) });

#[test]
fn mcp_is_switched_on_only_with_a_token_to_use_it() {
    let mut s = sample_session("mcp-enable");
    owner(&mut s);
    assert!(matches!(s.run("mcp.setEnabled", &json!({"on": true})), Err(CmdError::Params(_))));
    assert!(matches!(s.run("mcp.setEnabled", &json!({})), Err(CmdError::Params(_))));
    assert_eq!(s.run("mcp.setEnabled", &json!({"on": false})).unwrap(), json!({"on": false}));
    s.run("mcp.createToken", &json!({"name": "A"})).unwrap();
    assert_eq!(s.run("mcp.setEnabled", &json!({"on": true})).unwrap(), json!({"on": true}));
    assert!(s.mcp.settings.enabled);
}

#[test]
fn revoking_says_whether_a_token_was_there() {
    let mut s = sample_session("mcp-revoke");
    owner(&mut s);
    let id = s.run("mcp.createToken", &json!({"name": "A"})).unwrap()["id"].as_str().unwrap().to_string();
    assert_eq!(s.run("mcp.revokeToken", &json!({"id": "nothing"})).unwrap(), json!({"revoked": false}));
    assert_eq!(s.run("mcp.revokeToken", &json!({"id": id.clone()})).unwrap(), json!({"revoked": true}));
    assert_eq!(s.run("mcp.revokeToken", &json!({"id": id})).unwrap(), json!({"revoked": false}));
    assert!(s.mcp.settings.tokens.is_empty());
}
