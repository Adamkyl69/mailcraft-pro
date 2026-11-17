//! The pure parts of MCP: lines, tokens, the tool table, the argument checks.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::approval::{self, ApprovalRecord, ApprovalState, Verdict};
use super::protocol::{self, Id, Parsed};
use super::rules::{self, RuleFacts, RuleMiss};
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

/// RFC 6238 Appendix B's SHA-1 secret.
const RFC_SECRET: &[u8] = b"12345678901234567890";

#[test]
fn the_codes_of_rfc_4226_appendix_d() {
    let expected = [755_224, 287_082, 359_152, 969_429, 338_314, 254_676, 287_922, 162_583, 399_871, 520_489];
    for (counter, code) in expected.iter().enumerate() {
        assert_eq!(approval::hotp(RFC_SECRET, counter as u64), *code, "{counter}");
    }
}

#[test]
fn the_codes_of_rfc_6238_appendix_b_sha1() {
    // The RFC's eight digits, of which six are used: the last six.
    for (time, eight) in [
        (59, "94287082"),
        (1_111_111_109, "07081804"),
        (1_111_111_111, "14050471"),
        (1_234_567_890, "89005924"),
        (2_000_000_000, "69279037"),
        (20_000_000_000i64, "65353130"),
    ] {
        let want: u32 = eight[2..].parse().unwrap();
        assert_eq!(approval::code_at(RFC_SECRET, approval::step(time)), Some(want), "{time}");
    }
    assert_eq!(approval::step(59), 1);
    assert_eq!(approval::step(60), 2);
    assert_eq!(approval::step(-1), -1);
    assert_eq!(approval::code_at(RFC_SECRET, -1), None);
}

fn active() -> ApprovalRecord {
    ApprovalRecord { state: ApprovalState::Active, ..Default::default() }
}

fn code(time: i64) -> u32 {
    approval::code_at(RFC_SECRET, approval::step(time)).unwrap()
}

#[test]
fn a_code_is_taken_a_step_either_side_and_not_two() {
    let now = 1_111_111_111;
    for offset in [-30, 0, 30] {
        let mut rec = active();
        let verdict = approval::check(&mut rec, RFC_SECRET, code(now + offset), now);
        assert_eq!(verdict, Verdict::Ok { step: approval::step(now + offset) }, "{offset}");
        assert_eq!((rec.failures, rec.locked), (0, false));
    }
    for offset in [-60, 60, -3600] {
        let mut rec = active();
        assert_eq!(approval::check(&mut rec, RFC_SECRET, code(now + offset), now), Verdict::Wrong, "{offset}");
        assert_eq!(rec.failures, 1);
    }
}

#[test]
fn a_step_that_was_used_is_never_taken_again_even_after_a_restart() {
    let now = 1_234_567_890;
    let mut rec = active();
    let c = code(now);
    assert!(matches!(approval::check(&mut rec, RFC_SECRET, c, now), Verdict::Ok { .. }));
    assert_eq!(approval::check(&mut rec, RFC_SECRET, c, now), Verdict::Replayed);
    assert_eq!(approval::check(&mut rec, RFC_SECRET, c, now + 29), Verdict::Replayed);
    // What's kept in mcp.json carries the step: a restart doesn't give the code back.
    let kept: ApprovalRecord = serde_json::from_str(&serde_json::to_string(&rec).unwrap()).unwrap();
    let mut again = ApprovalRecord { failures: 0, ..kept };
    assert_eq!(approval::check(&mut again, RFC_SECRET, c, now), Verdict::Replayed);
    // The code before it, the step earlier, is also behind `last_step`.
    assert_eq!(approval::check(&mut again, RFC_SECRET, code(now - 30), now), Verdict::Replayed);
    // The next step's code is still good.
    assert!(matches!(approval::check(&mut again, RFC_SECRET, code(now + 30), now), Verdict::Ok { .. }));
}

#[test]
fn a_code_starting_with_zeros_is_compared_as_six_digits() {
    // 005924 at 1234567890.
    let mut rec = active();
    assert!(matches!(approval::check(&mut rec, RFC_SECRET, 5924, 1_234_567_890), Verdict::Ok { .. }));
    assert_eq!(approval::parse_code("005924"), Some(5924));
}

#[test]
fn five_wrong_codes_lock_and_a_locked_record_never_looks() {
    let now = 1_111_111_111;
    let mut rec = active();
    for n in 1..=5 {
        assert_eq!(approval::check(&mut rec, RFC_SECRET, 1, now), Verdict::Wrong, "{n}");
        assert_eq!(rec.locked, n == 5);
    }
    // A right code is refused while locked, and doesn't count.
    assert_eq!(approval::check(&mut rec, RFC_SECRET, code(now), now), Verdict::Locked);
    assert_eq!(rec.failures, 5);
    rec.locked = false;
    rec.failures = 0;
    assert!(matches!(approval::check(&mut rec, RFC_SECRET, code(now), now), Verdict::Ok { .. }));
}

#[test]
fn a_right_code_clears_the_wrong_ones_before_it() {
    let now = 1_111_111_111;
    let mut rec = active();
    for _ in 0..4 {
        approval::check(&mut rec, RFC_SECRET, 2, now);
    }
    assert!(matches!(approval::check(&mut rec, RFC_SECRET, code(now), now), Verdict::Ok { .. }));
    assert_eq!(rec.failures, 0);
}

#[test]
fn approval_that_isnt_set_up_takes_no_code() {
    let mut rec = ApprovalRecord::default();
    assert_eq!(approval::check(&mut rec, RFC_SECRET, code(59), 59), Verdict::NotSetUp);
    assert_eq!(rec.failures, 0);
}

#[test]
fn base32_is_rfc_4648s() {
    for (plain, coded) in
        [("", ""), ("f", "MY"), ("fo", "MZXQ"), ("foo", "MZXW6"), ("foob", "MZXW6YQ"), ("fooba", "MZXW6YTB"), ("foobar", "MZXW6YTBOI")]
    {
        assert_eq!(approval::base32(plain.as_bytes()), coded);
        assert_eq!(approval::unbase32(coded).as_deref(), Some(plain.as_bytes()), "{coded}");
        assert_eq!(approval::unbase32(&coded.to_lowercase()).as_deref(), Some(plain.as_bytes()));
    }
    assert_eq!(approval::unbase32("MZXW 6YTB oi==").as_deref(), Some(&b"foobar"[..]));
    assert_eq!(approval::base32(RFC_SECRET), "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
    assert_eq!(approval::unbase32("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ").as_deref(), Some(RFC_SECRET));
}

#[test]
fn text_that_isnt_base32_is_refused_and_so_is_a_secret_over_64_bytes() {
    for bad in ["1", "MZXW6!", "MZ-XW6", "é", "MZXW6\n"] {
        assert_eq!(approval::unbase32(bad), None, "{bad}");
    }
    assert!(approval::unbase32(&approval::base32(&[7u8; 64])).is_some());
    assert_eq!(approval::unbase32(&approval::base32(&[7u8; 65])), None);
    // Round trip of every length.
    for n in 0..40u8 {
        let bytes: Vec<u8> = (0..n).map(|i| i.wrapping_mul(37).wrapping_add(11)).collect();
        assert_eq!(approval::unbase32(&approval::base32(&bytes)), Some(bytes), "{n}");
    }
}

#[test]
fn the_authenticator_address_and_the_typed_code() {
    assert_eq!(approval::otpauth_uri("ABCD"), "otpauth://totp/MailCraft?secret=ABCD&issuer=MailCraft");
    for (text, want) in [
        ("123456", Some(123_456)),
        (" 123456 ", Some(123_456)),
        ("000001", Some(1)),
        ("12345", None),
        ("1234567", None),
        ("12 456", None),
        ("12345a", None),
        ("", None),
        ("+12345", None),
        ("١٢٣٤٥٦", None),
    ] {
        assert_eq!(approval::parse_code(text), want, "{text:?}");
    }
}

fn clean() -> RuleFacts {
    RuleFacts {
        rules_on: true,
        is_reply: true,
        owner_wrote_in_thread: true,
        from_receiving_account: true,
        unknown_recipients: Vec::new(),
        attachments: 0,
        checks: 0,
        sent_today: 0,
        daily_limit: 20,
    }
}

#[test]
fn a_clean_reply_needs_no_code_and_each_rule_missed_says_so() {
    assert_eq!(rules::misses(&clean()), []);
    let broken: [(RuleFacts, RuleMiss); 7] = [
        (RuleFacts { is_reply: false, ..clean() }, RuleMiss::NotAReply),
        (RuleFacts { owner_wrote_in_thread: false, ..clean() }, RuleMiss::OwnerNotInThread),
        (RuleFacts { from_receiving_account: false, ..clean() }, RuleMiss::OtherAccount),
        (RuleFacts { unknown_recipients: vec!["a@example.com".into(), "b@example.com".into()], ..clean() }, RuleMiss::UnknownRecipients { count: 2 }),
        (RuleFacts { attachments: 1, ..clean() }, RuleMiss::HasAttachments),
        (RuleFacts { checks: 1, ..clean() }, RuleMiss::SendChecks),
        (RuleFacts { sent_today: 20, ..clean() }, RuleMiss::DailyLimit),
    ];
    for (facts, miss) in broken {
        assert_eq!(rules::misses(&facts), std::slice::from_ref(&miss), "{miss:?}");
    }
    assert_eq!(rules::misses(&RuleFacts { rules_on: false, ..clean() }), [RuleMiss::RulesOff]);
    assert_eq!(rules::misses(&RuleFacts { daily_limit: 0, ..clean() }), [RuleMiss::DailyLimit]);
    // A new message has no thread or original account to speak of.
    assert_eq!(
        rules::misses(&RuleFacts { is_reply: false, owner_wrote_in_thread: false, from_receiving_account: false, ..clean() }),
        [RuleMiss::NotAReply]
    );
    // Several at once are all said.
    let many = RuleFacts { attachments: 2, sent_today: 99, ..clean() };
    assert_eq!(rules::misses(&many), [RuleMiss::HasAttachments, RuleMiss::DailyLimit]);
    assert_eq!(serde_json::to_value(RuleMiss::UnknownRecipients { count: 3 }).unwrap(), json!({"rule": "unknownRecipients", "count": 3}));
    assert_eq!(serde_json::to_value(RuleMiss::OwnerNotInThread).unwrap(), json!({"rule": "ownerNotInThread"}));
}
