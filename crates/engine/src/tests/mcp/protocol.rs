//! The JSON-RPC side through the session: tokens, methods, versions, the rate limit.

use super::*;

#[test]
fn a_notification_gets_no_answer_even_with_a_bad_token() {
    let (mut s, _) = mcp_session("mcp-notify", &ALL);
    let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    assert_eq!(s.mcp_handle("not a token", &note, tick()), McpOutcome::Reply(String::new()));
    let cancelled = json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 3}});
    assert_eq!(s.mcp_handle("", &cancelled, tick()), McpOutcome::Reply(String::new()));
}

#[test]
fn an_unknown_missing_or_off_token_is_refused() {
    let (mut s, t) = mcp_session("mcp-token", &ALL);
    let ping = |s: &mut Session, t: &str| call(s, t, "ping", json!({}));
    assert_eq!(ping(&mut s, &t)["result"], json!({}));
    for bad in ["", "mcp_wrong", &crate::mcp::tokens::make_token(&[8u8; 32]).unwrap()] {
        let r = ping(&mut s, bad);
        assert_eq!(r["error"]["code"], json!(-32001), "{bad}: {r}");
        assert!(!r["error"]["message"].as_str().unwrap().contains(bad) || bad.is_empty());
    }
    // Turned off, the right token is refused too.
    s.mcp.settings.enabled = false;
    assert_eq!(ping(&mut s, &t)["error"]["code"], json!(-32001));
    s.mcp.settings.enabled = true;
    assert_eq!(ping(&mut s, &t)["result"], json!({}));
}

#[test]
fn a_token_revoked_between_two_calls_is_refused_on_the_next() {
    let mut s = sample_session("mcp-revoked");
    s.mcp.random = Some(|n| Some(vec![9u8; n]));
    s.mcp.owner_ui = true;
    let id = s.run("mcp.createToken", &json!({"name": "A"})).unwrap()["id"].as_str().unwrap().to_string();
    let t = s.mcp.revealed.clone().unwrap().token;
    s.run("mcp.setEnabled", &json!({"on": true})).unwrap();
    assert_eq!(call(&mut s, &t, "ping", json!({}))["result"], json!({}));
    s.run("mcp.revokeToken", &json!({"id": id})).unwrap();
    assert_eq!(call(&mut s, &t, "ping", json!({}))["error"]["code"], json!(-32001));
}

#[test]
fn initialize_answers_the_version_asked_for_and_what_the_server_is() {
    let (mut s, t) = mcp_session("mcp-init", &ALL);
    for v in ["2025-06-18", "2025-03-26", "2024-11-05"] {
        let r = call(&mut s, &t, "initialize", json!({"protocolVersion": v, "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}));
        assert_eq!(r["result"]["protocolVersion"], json!(v));
        assert_eq!(r["result"]["serverInfo"]["name"], json!("mailcraft"));
        assert_eq!(r["result"]["capabilities"]["tools"]["listChanged"], json!(false));
        assert!(r["result"]["instructions"].is_string());
    }
    for asked in [json!({"protocolVersion": "1999-01-01"}), json!({}), json!(null)] {
        assert_eq!(call(&mut s, &t, "initialize", asked)["result"]["protocolVersion"], json!("2025-06-18"));
    }
}

#[test]
fn an_older_version_gets_no_structured_content_or_output_schemas() {
    let (mut s, t) = mcp_session("mcp-old", &ALL);
    // The newest has both.
    let newest = tool(&mut s, &t, "mail_accounts", json!({}));
    assert!(newest["structuredContent"]["accounts"].is_array());
    assert!(call(&mut s, &t, "tools/list", json!({}))["result"]["tools"][0]["outputSchema"].is_object());
    call(&mut s, &t, "initialize", json!({"protocolVersion": "2025-03-26"}));
    let old = tool(&mut s, &t, "mail_accounts", json!({}));
    assert_eq!(old["isError"], json!(false));
    assert!(old.get("structuredContent").is_none(), "{old}");
    assert!(old["content"][0]["text"].as_str().unwrap().contains("accounts"));
    let tools = call(&mut s, &t, "tools/list", json!({}));
    assert!(tools["result"]["tools"].as_array().unwrap().iter().all(|x| x.get("outputSchema").is_none()));
}

#[test]
fn methods_nobody_offered_are_not_found() {
    let (mut s, t) = mcp_session("mcp-methods", &ALL);
    for method in ["resources/list", "prompts/list", "completion/complete", "sampling/createMessage", "nope"] {
        assert_eq!(call(&mut s, &t, method, json!({}))["error"]["code"], json!(-32601), "{method}");
    }
}

#[test]
fn a_bad_request_is_answered_without_a_token_check() {
    let (mut s, _) = mcp_session("mcp-invalid", &ALL);
    let batch = json!([{"jsonrpc": "2.0", "id": 1, "method": "ping"}]);
    assert_eq!(reply(s.mcp_handle("x", &batch, tick()))["error"]["code"], json!(-32600));
    let array_params = json!({"jsonrpc": "2.0", "id": 5, "method": "tools/list", "params": [1]});
    let r = reply(s.mcp_handle("x", &array_params, tick()));
    assert_eq!((r["error"]["code"].clone(), r["id"].clone()), (json!(-32602), json!(5)));
}

#[test]
fn tools_call_needs_a_tool_name_and_object_arguments() {
    let (mut s, t) = mcp_session("mcp-call", &ALL);
    assert_eq!(call(&mut s, &t, "tools/call", json!({}))["error"]["code"], json!(-32602));
    assert_eq!(call(&mut s, &t, "tools/call", json!({"name": 5}))["error"]["code"], json!(-32602));
    assert_eq!(call(&mut s, &t, "tools/call", json!({"name": "mail_nope"}))["error"]["code"], json!(-32602));
    assert_eq!(call(&mut s, &t, "tools/call", json!({"name": "mail_list", "arguments": [1]}))["error"]["code"], json!(-32602));
    // No arguments at all is the empty object: a tool error, not a protocol one.
    let r = call(&mut s, &t, "tools/call", json!({"name": "mail_list"}));
    assert!(fails(&r["result"]).contains("`account`"));
}

#[test]
fn the_eleventh_call_in_a_second_is_asked_to_slow_down() {
    let (mut s, t) = mcp_session("mcp-rate", &ALL);
    let ping = request(1, "ping", json!({}));
    for i in 0..10 {
        let r = reply(s.mcp_handle(&t, &ping, 50_000 + i));
        assert_eq!(r["result"], json!({}), "{i}");
    }
    let r = reply(s.mcp_handle(&t, &ping, 50_500));
    assert_eq!(r["error"]["code"], json!(-32002), "{r}");
    // A second later it's open again.
    assert_eq!(reply(s.mcp_handle(&t, &ping, 51_100))["result"], json!({}));
}

#[test]
fn a_token_with_a_call_still_waiting_cant_start_another() {
    let mut s = big_projects(&temp("mcp-busy"), 3);
    let t = token();
    s.mcp_seed_token(&t, &ALL);
    let key = s.list.first().map(|m| m.key.to_id()).unwrap();
    let ask = request(1, "tools/call", json!({"name": "mail_read", "arguments": {"key": key}}));
    let McpOutcome::Later(ticket) = s.mcp_handle(&t, &ask, 10_000) else { panic!("it should wait for the body") };
    assert!(s.mcp_waiting());
    let again = reply(s.mcp_handle(&t, &request(2, "ping", json!({})), 12_000));
    assert_eq!(again["error"]["code"], json!(-32002), "{again}");
    s.mcp_forget(ticket);
    assert!(!s.mcp_waiting());
    assert_eq!(reply(s.mcp_handle(&t, &request(3, "ping", json!({})), 14_000))["result"], json!({}));
}

#[test]
fn a_call_that_is_made_is_saved_once_a_minute_in_the_settings() {
    let dir = scratch("mcp-minute");
    let store = Store::open(dir.clone()).unwrap();
    assert!(sample::seed_if_empty(&store, NOW).unwrap());
    let mut s = Session::open(Some(store), mailcraft_model::dates::Clock::new(NOW, 0));
    s.mcp.random = Some(|n| Some(vec![3u8; n]));
    s.mcp.owner_ui = true;
    s.run("mcp.createToken", &json!({"name": "Mail helper", "permissions": ["read"]})).unwrap();
    let t = s.mcp.revealed.clone().unwrap().token;
    s.run("mcp.setEnabled", &json!({"on": true})).unwrap();
    let kept = |dir: &std::path::Path| serde_json::from_str::<Value>(&std::fs::read_to_string(dir.join("mcp.json")).unwrap()).unwrap();
    assert_eq!(kept(&dir)["tokens"][0]["lastUsed"], json!(null));
    reply(s.mcp_handle(&t, &request(1, "ping", json!({})), 120_000 * 3 + 7_000));
    // Minute precision: the minute the call was in.
    assert_eq!(kept(&dir)["tokens"][0]["lastUsed"], json!(360));
    // The token itself is nowhere in the file, only its hash.
    let file = std::fs::read_to_string(dir.join("mcp.json")).unwrap();
    assert!(!file.contains(&t) && file.contains(&crate::mcp::tokens::hash_hex(&t)));
}

#[test]
fn settings_with_unknown_fields_or_a_newer_version_are_read_and_the_extras_dropped_on_saving() {
    let dir = scratch("mcp-future");
    std::fs::create_dir_all(&dir).unwrap();
    let hash = crate::mcp::tokens::hash_hex(&token());
    let doc = json!({
        "version": 9, "enabled": true, "somethingNew": [1, 2],
        "tokens": [
            {"id": &hash[..12], "name": "Old", "hash": hash, "permissions": ["organize"], "created": 5, "lastUsed": null, "extra": true},
            {"id": "bad", "name": "Broken", "hash": "zz", "permissions": [], "created": 5, "lastUsed": null},
        ],
    });
    std::fs::write(dir.join("mcp.json"), doc.to_string()).unwrap();
    let store = Store::open(dir.clone()).unwrap();
    let mut s = Session::open(Some(store), mailcraft_model::dates::Clock::new(NOW, 0));
    assert!(s.mcp.settings.enabled);
    // The record that can't be one is left out; the other holds Read whatever it says.
    assert_eq!(s.mcp.settings.tokens.len(), 1);
    assert!(s.mcp.settings.tokens[0].permissions.contains(&Permission::Read));
    assert_eq!(call(&mut s, &token(), "ping", json!({}))["result"], json!({}));
    s.mcp.owner_ui = true;
    s.run("mcp.setEnabled", &json!({"on": true})).unwrap();
    let saved = std::fs::read_to_string(dir.join("mcp.json")).unwrap();
    assert!(!saved.contains("somethingNew") && !saved.contains("extra") && saved.contains("\"version\":1"), "{saved}");
}

#[test]
fn a_damaged_settings_file_leaves_no_tokens_and_is_kept_aside() {
    let dir = scratch("mcp-damaged");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("mcp.json"), "{ this isn't json").unwrap();
    let store = Store::open(dir.clone()).unwrap();
    let mut s = Session::open(Some(store), mailcraft_model::dates::Clock::new(NOW, 0));
    assert!(s.mcp.settings.tokens.is_empty() && !s.mcp.settings.enabled);
    assert_eq!(call(&mut s, &token(), "ping", json!({}))["error"]["code"], json!(-32001));
    // The store kept the damaged copy, and didn't write over what it couldn't read.
    assert!(dir.join("mcp.json.bad").exists());
}

/// The app is told, at `initialize`, what to do with a `needsCode` answer.
#[test]
fn initialize_tells_the_app_to_ask_the_user_for_the_code() {
    let (mut s, t) = mcp_session("mcp-init-instructions", &ALL);
    let r = call(&mut s, &t, "initialize", json!({"protocolVersion": "2025-06-18"}));
    let text = r["result"]["instructions"].as_str().unwrap();
    for needle in ["needsCode", "ask the user for the 6-digit code", "authenticator app", "Never guess a code or reuse"] {
        assert!(text.contains(needle), "{needle}: {text}");
    }
}
