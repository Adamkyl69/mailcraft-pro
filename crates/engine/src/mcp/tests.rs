//! The pure parts of MCP: lines, tokens, the tool table, the argument checks.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::protocol::{self, Id, Parsed};
use super::tokens::{self, RateWindow};
use super::tools::{self, Args, RUNNABLE, TOOLS};
use super::{MAX_DEPTH, PROTOCOL_VERSIONS, Permission};

fn invalid(line: &str) -> (Option<Id>, i32) {
    match protocol::parse_line(line) {
        Parsed::Invalid { id, code, .. } => (id, code),
        Parsed::Request(r) => panic!("accepted {r:?}"),
    }
}

/// `{"a":{"a":...}}` with `n` objects.
fn nested(n: usize) -> Value {
    let mut v = json!(1);
    for _ in 1..n {
        v = json!({"a": v});
    }
    v
}

#[test]
fn a_line_that_isnt_json_is_a_parse_error_with_no_id() {
    assert_eq!(invalid("not json"), (None, protocol::PARSE_ERROR));
    assert_eq!(invalid("{\"jsonrpc\":"), (None, protocol::PARSE_ERROR));
    assert_eq!(invalid(""), (None, protocol::PARSE_ERROR));
}

#[test]
fn a_batch_is_refused() {
    assert_eq!(invalid(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#), (None, protocol::INVALID_REQUEST));
    assert_eq!(invalid("42"), (None, protocol::INVALID_REQUEST));
}

#[test]
fn the_version_and_the_method_are_checked_and_the_id_answered_when_readable() {
    assert_eq!(invalid(r#"{"id":4,"method":"ping"}"#), (Some(Id::Num(4)), protocol::INVALID_REQUEST));
    assert_eq!(invalid(r#"{"jsonrpc":"1.0","id":"x","method":"ping"}"#), (Some(Id::Str("x".into())), protocol::INVALID_REQUEST));
    assert_eq!(invalid(r#"{"jsonrpc":"2.0","id":4}"#), (Some(Id::Num(4)), protocol::INVALID_REQUEST));
    let long = "m".repeat(129);
    assert_eq!(invalid(&format!(r#"{{"jsonrpc":"2.0","id":4,"method":"{long}"}}"#)).1, protocol::INVALID_REQUEST);
}

#[test]
fn only_a_short_string_or_an_integer_is_an_id() {
    for bad in ["1.5", "true", "null", "{}", "[1]", &format!("\"{}\"", "i".repeat(257))] {
        let line = format!(r#"{{"jsonrpc":"2.0","id":{bad},"method":"ping"}}"#);
        assert_eq!(invalid(&line), (None, protocol::INVALID_REQUEST), "{bad}");
    }
    for good in ["7", "-3", "\"abc\"", &format!("\"{}\"", "i".repeat(256))] {
        let line = format!(r#"{{"jsonrpc":"2.0","id":{good},"method":"ping"}}"#);
        assert!(matches!(protocol::parse_line(&line), Parsed::Request(r) if r.id.is_some()), "{good}");
    }
}

#[test]
fn a_request_without_an_id_is_a_notification() {
    let Parsed::Request(r) = protocol::parse_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#) else { panic!("refused") };
    assert_eq!((r.id, r.params), (None, Value::Null));
}

#[test]
fn params_are_an_object_or_nothing() {
    for bad in ["[1]", "\"x\"", "3", "true"] {
        let line = format!(r#"{{"jsonrpc":"2.0","id":2,"method":"ping","params":{bad}}}"#);
        assert_eq!(invalid(&line), (Some(Id::Num(2)), protocol::INVALID_PARAMS), "{bad}");
    }
    for good in ["{}", "null"] {
        let line = format!(r#"{{"jsonrpc":"2.0","id":2,"method":"ping","params":{good}}}"#);
        assert!(matches!(protocol::parse_line(&line), Parsed::Request(_)), "{good}");
    }
}

#[test]
fn nesting_over_64_is_refused_and_64_accepted() {
    let request = |depth: usize| json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"arguments": nested(depth)}});
    // The request is two deep before its arguments' own nesting starts at the third level.
    let at_limit = MAX_DEPTH - 2;
    assert!(matches!(protocol::parse_value(&request(at_limit)), Parsed::Request(_)));
    assert!(matches!(protocol::parse_value(&request(at_limit + 1)), Parsed::Invalid { code: protocol::INVALID_REQUEST, .. }));
    assert_eq!(protocol::depth(&nested(100)), 100);
    assert_eq!(protocol::depth(&json!(1)), 1);
    assert_eq!(protocol::depth(&json!([[], [[]]])), 3);
}

#[test]
fn versions_are_negotiated() {
    for v in PROTOCOL_VERSIONS {
        assert_eq!(protocol::negotiate(Some(v)), v);
    }
    assert_eq!(protocol::negotiate(Some("1999-01-01")), "2025-06-18");
    assert_eq!(protocol::negotiate(None), "2025-06-18");
}

#[test]
fn answers_carry_the_id_as_given() {
    let ok: Value = serde_json::from_str(&protocol::result(&Id::Str("a-1".into()), json!({"x": 1}))).unwrap();
    assert_eq!(ok, json!({"jsonrpc": "2.0", "id": "a-1", "result": {"x": 1}}));
    let err: Value = serde_json::from_str(&protocol::error(None, protocol::PARSE_ERROR, "no")).unwrap();
    assert_eq!(err, json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "no"}}));
    let err: Value = serde_json::from_str(&protocol::error(Some(&Id::Num(9)), protocol::SLOW_DOWN, "slow")).unwrap();
    assert_eq!(err["id"], json!(9));
}

#[test]
fn tokens_have_a_fixed_shape() {
    let token = tokens::make_token(&[7u8; 32]).unwrap();
    assert!(token.starts_with("mcp_") && token.len() == 47 && tokens::looks_like_token(&token), "{token}");
    assert_eq!(tokens::make_token(&[7u8; 31]), None);
    assert_eq!(tokens::make_token(&[7u8; 33]), None);
    assert_ne!(token, tokens::make_token(&[8u8; 32]).unwrap());
    for bad in ["", "mcp_", "mcp_short", "xxx_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", &format!("mcp_{}+", "A".repeat(42)), &format!("{token}A")]
    {
        assert!(!tokens::looks_like_token(bad), "{bad}");
    }
    assert_eq!(tokens::b64url(&[0xfb, 0xff]), "-_8");
}

#[test]
fn a_token_is_kept_as_its_sha256() {
    // FIPS 180-2's test vector for "abc".
    assert_eq!(tokens::hash_hex("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert_eq!(tokens::hash_hex("abc").len(), 64);
}

#[test]
fn comparing_needs_the_same_bytes() {
    assert!(tokens::same(b"abc", b"abc"));
    assert!(!tokens::same(b"abc", b"abd"));
    assert!(!tokens::same(b"abc", b"abcd"));
    assert!(tokens::same(b"", b""));
}

#[test]
fn ten_calls_a_second_are_allowed_and_the_eleventh_isnt() {
    let mut w = RateWindow::default();
    assert!((0..10).all(|i| w.allow(5000 + i)));
    assert!(!w.allow(5500));
    assert!(!w.allow(5999));
    // A second after the first, there's room for one.
    assert!(w.allow(6000));
    assert!(!w.allow(6000));
    // A clock that went back doesn't wedge it.
    assert!(w.allow(0));
}

fn perms(list: &[Permission]) -> BTreeSet<Permission> {
    list.iter().copied().collect()
}

#[test]
fn only_the_tools_a_token_may_use_are_listed() {
    let names = |p: &[Permission], structured| -> Vec<String> {
        tools::listing(&perms(p), structured).as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(names(&[Permission::Read], true), ["mail_accounts", "mail_list", "mail_search", "mail_read", "mail_commands", "monitor_log"]);
    assert_eq!(names(&[Permission::Read, Permission::Organize], true).len(), 8);
    assert!(names(&[], true).is_empty());
    assert!(names(&[Permission::Organize], true).contains(&"mail_run".to_string()));
}

#[test]
fn schemas_forbid_extra_arguments_and_output_schemas_follow_the_version() {
    let all = perms(&[Permission::Read, Permission::Organize, Permission::Send]);
    for t in tools::listing(&all, true).as_array().unwrap() {
        assert_eq!(t["inputSchema"]["additionalProperties"], json!(false), "{}", t["name"]);
        assert!(t["outputSchema"].is_object() && t["annotations"]["destructiveHint"] == json!(false), "{}", t["name"]);
    }
    for t in tools::listing(&all, false).as_array().unwrap() {
        assert!(t.get("outputSchema").is_none(), "{}", t["name"]);
    }
    for t in &TOOLS {
        assert_eq!(tools::find(t.name).map(|f| f.name), Some(t.name));
    }
    assert!(tools::find("mail_delete").is_none());
}

#[test]
fn what_mail_run_may_run_is_nine_commands_that_can_be_undone() {
    assert_eq!(RUNNABLE.len(), 9);
    for id in RUNNABLE {
        assert!(id.starts_with("mail."), "{id}");
    }
    for denied in ["mail.delete", "mail.archive", "mail.moveToFocused", "mail.moveToOther", "mail.empty", "folder.empty"] {
        assert!(!RUNNABLE.contains(&denied), "{denied}");
    }
}

#[test]
fn arguments_are_checked_with_their_limits() {
    let v = json!({"s": "abc", "n": 5, "f": 1.5, "b": true, "l": ["x", "y"], "o": {"k": 1}, "big": "x".repeat(70_000)});
    let a = Args::new(&v, &["s", "n", "f", "b", "l", "o", "big", "missing"]).unwrap();
    assert_eq!(a.str("s", 10), Ok(Some("abc")));
    assert_eq!(a.str("missing", 10), Ok(None));
    assert!(a.str("s", 2).unwrap_err().contains("`s`"));
    assert!(a.str("n", 10).unwrap_err().contains("must be a string"));
    assert!(a.str("big", super::MAX_STRING).unwrap_err().contains("too long"));
    assert!(a.req_str("missing", 5).unwrap_err().contains("required"));
    assert_eq!(a.int("n", 1, 10), Ok(Some(5)));
    assert!(a.int("n", 6, 10).unwrap_err().contains("from 6 to 10"));
    assert!(a.int("f", 0, 10).unwrap_err().contains("whole number"));
    assert!(a.int("s", 0, 10).is_err());
    assert_eq!(a.bool("b"), Ok(Some(true)));
    assert!(a.bool("n").is_err());
    assert_eq!(a.strings("l", 1, 2), Ok(Some(vec!["x", "y"])));
    assert!(a.strings("l", 3, 5).is_err() && a.strings("l", 0, 1).is_err() && a.strings("s", 0, 5).is_err());
    assert!(a.object("o").unwrap().is_some() && a.object("l").is_err());
    assert_eq!(a.names().count(), 7);
}

#[test]
fn an_unknown_argument_or_arguments_that_arent_an_object_are_refused() {
    assert!(Args::new(&json!({"x": 1}), &["y"]).unwrap_err().contains("unknown argument `x`"));
    assert!(Args::new(&json!([1]), &[]).is_err());
    let long = "q".repeat(500);
    let e = Args::new(&json!({ long.clone(): 1 }), &[]).unwrap_err();
    assert!(e.len() < 100, "{e}");
    // A float that's far too large is no integer.
    assert!(Args::new(&json!({"limit": 1e300}), &["limit"]).unwrap().int("limit", 1, 200).is_err());
}
