//! JSON-RPC 2.0 as MCP speaks it: one request per line, checked before anything acts on it.

use serde_json::{Value, json};

use super::{MAX_DEPTH, PROTOCOL_VERSIONS};

pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;
/// An unknown or revoked token, or MCP switched off.
pub const TOKEN_REFUSED: i32 = -32001;
/// Over 10 a second, or a call already running for this token.
pub const SLOW_DOWN: i32 = -32002;
/// The bridge only: MailCraft isn't running.
pub const NOT_RUNNING: i32 = -32000;

/// The longest method name, bytes.
const MAX_METHOD: usize = 128;
/// The longest string id, bytes.
const MAX_ID: usize = 256;

/// A request's id, answered as it was given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Id {
    Num(i64),
    Str(String),
}

impl Id {
    pub fn to_value(&self) -> Value {
        match self {
            Id::Num(n) => json!(n),
            Id::Str(s) => json!(s),
        }
    }
}

/// A request that passed the checks (`id` is `None` for a notification).
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub id: Option<Id>,
    pub method: String,
    /// `Null` when there were none.
    pub params: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Parsed {
    Request(Request),
    Invalid { id: Option<Id>, code: i32, message: String },
}

fn invalid(id: Option<Id>, code: i32, message: &str) -> Parsed {
    Parsed::Invalid { id, code, message: message.to_string() }
}

/// The id when it is one that can be answered to: a string of at most 256 bytes, or an integer.
fn readable_id(v: &Value) -> Option<Id> {
    match v {
        Value::String(s) if s.len() <= MAX_ID => Some(Id::Str(s.clone())),
        Value::Number(n) => n.as_i64().map(Id::Num),
        _ => None,
    }
}

/// How deep `v` nests (a scalar is 1). Walked with a stack of its own, never by recursion.
pub fn depth(v: &Value) -> usize {
    let mut deepest = 0;
    let mut stack = vec![(v, 1usize)];
    while let Some((v, d)) = stack.pop() {
        deepest = deepest.max(d);
        match v {
            Value::Array(a) => stack.extend(a.iter().map(|x| (x, d.saturating_add(1)))),
            Value::Object(o) => stack.extend(o.values().map(|x| (x, d.saturating_add(1)))),
            _ => {}
        }
    }
    deepest
}

/// The request in a JSON value (the socket hands requests over parsed).
pub fn parse_value(v: &Value) -> Parsed {
    let Some(object) = v.as_object() else {
        return match v {
            Value::Array(_) => invalid(None, INVALID_REQUEST, "batches aren't supported"),
            _ => invalid(None, INVALID_REQUEST, "a request is a JSON object"),
        };
    };
    let id = object.get("id").and_then(readable_id);
    if depth(v) > MAX_DEPTH {
        return invalid(id, INVALID_REQUEST, "the request nests too deeply");
    }
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return invalid(id, INVALID_REQUEST, "jsonrpc must be \"2.0\"");
    }
    let Some(method) = object.get("method").and_then(Value::as_str).filter(|m| m.len() <= MAX_METHOD) else {
        return invalid(id, INVALID_REQUEST, "method must be a string of at most 128 bytes");
    };
    let id = match object.get("id") {
        None => None,
        Some(given) => match readable_id(given) {
            Some(id) => Some(id),
            None => return invalid(None, INVALID_REQUEST, "id must be a string of at most 256 bytes or an integer"),
        },
    };
    let params = match object.get("params") {
        None | Some(Value::Null) => Value::Null,
        Some(p @ Value::Object(_)) => p.clone(),
        Some(_) => return invalid(id, INVALID_PARAMS, "params must be an object"),
    };
    Parsed::Request(Request { id, method: method.to_string(), params })
}

/// The request on one line (the fuzz target and the bridge).
pub fn parse_line(line: &str) -> Parsed {
    match serde_json::from_str::<Value>(line.trim()) {
        Ok(v) => parse_value(&v),
        Err(_) => invalid(None, PARSE_ERROR, "that isn't JSON"),
    }
}

/// `asked` when it's a version spoken, else the newest.
pub fn negotiate(asked: Option<&str>) -> &'static str {
    asked.and_then(|a| PROTOCOL_VERSIONS.iter().find(|v| **v == a).copied()).unwrap_or(PROTOCOL_VERSIONS[0])
}

/// The response line (no newline) to a request.
pub fn result(id: &Id, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id.to_value(), "result": result}).to_string()
}

/// The error line (no newline); the id is `null` when it isn't known.
pub fn error(id: Option<&Id>, code: i32, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id.map_or(Value::Null, Id::to_value), "error": {"code": code, "message": message}}).to_string()
}
