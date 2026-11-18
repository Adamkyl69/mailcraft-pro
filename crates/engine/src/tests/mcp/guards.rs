//! A panic inside a tool or while a waiting call is decided answers an error: the app never
//! crashes, the token isn't left busy and other waiting calls stay.

use super::*;

/// A tool that panics is an error for the app, and the next call works.
#[test]
fn a_tool_that_panics_answers_an_error() {
    let (mut s, t) = mcp_session("mcp-guard-tool", &ALL);
    s.mcp.panic_in = Some("mail_accounts");
    let r = tool(&mut s, &t, "mail_accounts", json!({}));
    assert_eq!(fails(&r), "the tool failed unexpectedly");
    s.mcp.panic_in = None;
    ok(&tool(&mut s, &t, "mail_accounts", json!({})));
}

/// A panic while a waiting call is decided answers that call, frees its token and keeps the rest.
#[test]
fn a_poll_that_panics_answers_the_call_and_frees_its_token() {
    let mut s = big_projects(&temp("mcp-guard-poll"), 3);
    let t = token();
    s.mcp_seed_token(&t, &ALL);
    let key = s.list.first().map(|m| m.key.to_id()).unwrap();
    let ask = request(1, "tools/call", json!({"name": "mail_read", "arguments": {"key": key}}));
    assert!(matches!(s.mcp_handle(&t, &ask, 10_000), McpOutcome::Later(_)));
    s.mcp.panic_in = Some("poll");
    let done = s.mcp_finished(11_000);
    assert_eq!(done.len(), 1);
    assert!(done[0].1.contains("the call failed unexpectedly"), "{}", done[0].1);
    assert!(!s.mcp_waiting());
    s.mcp.panic_in = None;
    assert_eq!(reply(s.mcp_handle(&t, &request(3, "ping", json!({})), 14_000))["result"], json!({}));
}
