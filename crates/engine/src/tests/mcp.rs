//! MCP against the sample mailbox: JSON-RPC requests handed to `Session::mcp_handle`, as the
//! app's socket hands them, and what the session does and leaves alone.

use std::cell::Cell;

use mailcraft_model::{MessageKey, SyncStatus};

use super::*;
use crate::McpOutcome;
use crate::mcp::Permission;

mod adversarial;
mod approval;
mod edge_cases;
mod permissions;
mod protocol;
mod sending;
mod tools;

/// A token (the same in every test: each session has its own records).
fn token() -> String {
    crate::mcp::tokens::make_token(&[7u8; 32]).unwrap()
}

thread_local!(static NOW_MS: Cell<i64> = const { Cell::new(1_000_000) });

/// A time two seconds after the last asked: a test that isn't about the rate limit never meets it.
fn tick() -> i64 {
    NOW_MS.with(|n| {
        n.set(n.get() + 2000);
        n.get()
    })
}

/// The sample mailbox, and a token with `perms` in it.
fn mcp_session(name: &str, perms: &[Permission]) -> (Session, String) {
    let mut s = sample_session(name);
    let t = token();
    s.mcp_seed_token(&t, perms);
    (s, t)
}

const ALL: [Permission; 3] = [Permission::Read, Permission::Organize, Permission::Send];

fn request(id: i64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

/// The answer line as JSON; a call that waits for the network is a failure of the test.
fn reply(out: McpOutcome) -> Value {
    match out {
        McpOutcome::Reply(line) => serde_json::from_str(&line).unwrap_or(Value::Null),
        McpOutcome::Later(t) => panic!("the call waits (ticket {t})"),
    }
}

fn call(s: &mut Session, t: &str, method: &str, params: Value) -> Value {
    reply(s.mcp_handle(t, &request(1, method, params), tick()))
}

/// A `tools/call` answer's `result`.
fn tool(s: &mut Session, t: &str, name: &str, args: Value) -> Value {
    call(s, t, "tools/call", json!({"name": name, "arguments": args}))["result"].clone()
}

/// [`tool`] at a time of the test's choosing (a code is good for a time).
fn tool_at(s: &mut Session, t: &str, name: &str, args: Value, now_ms: i64) -> Value {
    reply(s.mcp_handle(t, &request(1, "tools/call", json!({"name": name, "arguments": args})), now_ms))["result"].clone()
}

/// What a tool answered, which must not be an error.
fn ok(r: &Value) -> &Value {
    assert_eq!(r["isError"], json!(false), "{r}");
    &r["structuredContent"]
}

/// A tool's error text.
fn fails(r: &Value) -> String {
    assert_eq!(r["isError"], json!(true), "{r}");
    r["content"][0]["text"].as_str().unwrap().to_string()
}

/// The keys of the sample's Inbox, newest first.
fn inbox(s: &mut Session) -> Vec<MessageKey> {
    s.folder_state("sample", "INBOX").messages.iter().map(|m| m.key.clone()).collect()
}

/// What the user sees, to compare before and after a call.
#[derive(Debug, PartialEq)]
struct Seen {
    view: View,
    search: Option<(String, SearchScope)>,
    list: Vec<MessageKey>,
    selection: Vec<MessageKey>,
    focus: Option<MessageKey>,
    reading: Option<crate::Reading>,
    compose: Option<u64>,
    filter: crate::ListFilter,
}

fn seen(s: &Session) -> Seen {
    Seen {
        view: s.view.clone(),
        search: s.search.clone(),
        list: s.list.iter().map(|m| m.key.clone()).collect(),
        selection: s.selection.clone(),
        focus: s.focus.clone(),
        reading: s.reading.clone(),
        compose: s.compose.as_ref().map(|c| c.id),
        filter: s.filter,
    }
}

/// A user in the middle of things: Projects/Short film open with a message read in the pane and another
/// message being written.
fn busy_user(s: &mut Session) {
    s.run("folder.open", &json!({"account": "sample", "folder": "Projects/Short film"})).unwrap();
    let first = s.list.first().map(|m| m.key.to_id()).unwrap();
    s.run("list.open", &json!({"key": first})).unwrap();
    s.run("mail.new", &json!({})).unwrap();
}

/// The server's word that an account is `status`.
fn status(account: &str, status: SyncStatus) -> Event {
    Event::Status { account: account.into(), status }
}
