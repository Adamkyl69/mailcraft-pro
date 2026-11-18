//! The session's side of MCP: what a local AI app asks, checked and answered
//! (`docs/specs/mcp-server-build.md`). The pure parts (JSON-RPC lines, tokens, the tool table) are
//! the top-level `crate::mcp`; this is what acts on the session. The app reads the clock and
//! carries the asks (`apps/mailcraft/src/startup/single_instance/mcp.rs`): the engine only gets
//! the time as `now_ms`.
//!
//! Nothing here logs a token, a code, an address, a subject, a key or an argument: one line a
//! call says who (the token's name), which tool and how it went.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde_json::{Value, json};

use super::*;
use crate::mcp::approval::{self, ApprovalState};
use crate::mcp::protocol::{self, Id, Parsed};
use crate::mcp::tokens::{self, McpSettings, RateWindow, TokenRecord};
use crate::mcp::tools::{self, Args};
use crate::mcp::{
    APPROVAL_SECRET, BridgeCommand, LogLine, LogQuery, MAX_TOKENS, PROTOCOL_VERSIONS, Permission, READ_WAIT_MS, SEND_WAIT_MS, SYNC_WAIT_MS,
};

mod aside;
mod facts;
mod organize;
mod reading;
mod sending;

/// The document the settings are kept in.
const MCP_DOC: &str = "mcp.json";
/// The longest error a server gave that's passed on to an AI app.
const MAX_ERROR: usize = 200;

/// What a call comes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpOutcome {
    /// One JSON line to answer with, or `""` for a notification (nothing is written).
    Reply(String),
    /// The answer waits for the network: a ticket, to ask for with [`Session::mcp_finished`].
    Later(u64),
}

/// A token just made, shown once on the card until it's dismissed.
#[derive(Clone)]
pub struct Revealed {
    pub name: String,
    pub token: String,
}

impl std::fmt::Debug for Revealed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Revealed({:?}, <token>)", self.name)
    }
}

/// An approval being set up: the secret, shown as a QR code and as text until a code from the
/// authenticator app confirms it. Never saved or logged.
#[derive(Clone)]
pub struct ApprovalSetup {
    /// The secret in base32, for the card.
    pub secret_b32: String,
    /// The `otpauth://` address the QR code holds.
    pub uri: String,
    pub(crate) bytes: Vec<u8>,
}

impl std::fmt::Debug for ApprovalSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApprovalSetup(<secret>)")
    }
}

/// A send waiting for a code (or for Send or Discard in the app): the message as it was frozen.
#[derive(Clone)]
pub struct PendingSend {
    /// 16 hex characters. Approving needs the code as well, so it isn't a secret on its own.
    pub id: String,
    /// Only this token may approve it.
    pub token_id: String,
    pub compose: Compose,
    pub created_ms: i64,
    pub expires_ms: i64,
    /// What the call answered: who it's to, the subject, the warnings, why a code is needed.
    pub summary: Value,
}

impl std::fmt::Debug for PendingSend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PendingSend({})", self.id)
    }
}

/// What deciding a waiting call came to.
enum Polled {
    Refused,
    Waiting,
    Answer(Result<Value, String>),
}

/// MCP's state in the session.
#[derive(Default)]
pub struct McpState {
    /// What `mcp.json` keeps.
    pub settings: McpSettings,
    /// A token just made, shown once on the card until dismissed.
    pub revealed: Option<Revealed>,
    /// Approval being set up, until a code confirms it. Never saved.
    pub setup: Option<ApprovalSetup>,
    /// Sends waiting for a code (or Send or Discard in the app). In memory only.
    pub pending: Vec<PendingSend>,
    /// The approval secret, read from the keychain. Never saved here, never logged.
    pub(crate) secret: Option<Vec<u8>>,
    /// The system's secure random source, set by the app (`None`: tokens can't be made).
    pub random: Option<fn(usize) -> Option<Vec<u8>>>,
    /// This run's WARN and ERROR lines, set by the app (`None`: `monitor_log` has none).
    pub log_source: Option<fn(&LogQuery) -> Vec<LogLine>>,
    /// How AI apps start the bridge, set by the app (`None`: the card shows no path).
    pub bridge: Option<BridgeCommand>,
    /// True only while the window dispatches a click from the Connected apps card.
    pub owner_ui: bool,
    /// `mcp.json` couldn't be read: MCP is off this run and the file is never saved over.
    unread: bool,
    /// Tests and the fuzz target seed tokens in memory: nothing is saved.
    memory_only: bool,
    /// The last calls of each token (by token id).
    rates: HashMap<String, RateWindow>,
    /// The calls of every unknown or malformed token together, for the same limit.
    bad_rate: RateWindow,
    /// When a refused unknown token was last logged, and how many were refused since.
    bad_logged: Option<i64>,
    bad_skipped: u32,
    /// Token ids with a call still waiting for the network.
    busy: HashSet<String>,
    /// Calls waiting for the network.
    waiting: Vec<Deferred>,
    next_ticket: u64,
    /// The protocol version each token's app asked for at `initialize` (calls come one to a
    /// connection, so this is where it's remembered).
    versions: HashMap<String, &'static str>,
    /// Tests only: the tool (or `poll`) that panics, to prove the guard around them.
    #[cfg(test)]
    pub(crate) panic_in: Option<&'static str>,
}

impl std::fmt::Debug for McpState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpState").field("tokens", &self.settings.tokens.len()).field("pending", &self.pending.len()).finish_non_exhaustive()
    }
}

/// A call waiting for the network.
struct Deferred {
    ticket: u64,
    token_id: String,
    tool: &'static str,
    started_ms: i64,
    deadline_ms: i64,
    id: Id,
    structured: bool,
    kind: Waiting,
}

/// What a call waits for.
enum Waiting {
    /// `mail_sync`: each account asked, and whether it has answered.
    Sync { accounts: BTreeMap<String, bool>, failed: BTreeMap<String, String> },
    /// `mail_read`: a message's source being downloaded.
    Body { key: MessageKey, args: Value, outcome: Option<Result<(), String>> },
    /// `mail_send`: the server's word on a message handed over.
    Send { compose_id: u64, outcome: Option<Result<SendEnd, String>> },
}

/// How a send that was handed over ended.
enum SendEnd {
    Sent,
    /// Kept in the Outbox to go later, and why.
    Queued(String),
}

/// How a tool came out: an answer, or waiting for the network.
enum Step {
    Done(Result<Value, String>),
    /// Not allowed (a limit, a lock, no approval): said as a tool error, and logged as refused.
    Refused(String),
    Later(Waiting),
}

impl From<Result<Value, String>> for Step {
    fn from(r: Result<Value, String>) -> Self {
        Step::Done(r)
    }
}

/// Time a call took, where there's a clock to read (the browser's engine never runs this).
struct Watch {
    #[cfg(not(target_arch = "wasm32"))]
    started: std::time::Instant,
}

impl Watch {
    fn start() -> Watch {
        Watch {
            #[cfg(not(target_arch = "wasm32"))]
            started: std::time::Instant::now(),
        }
    }

    fn ms(&self) -> u128 {
        #[cfg(not(target_arch = "wasm32"))]
        return self.started.elapsed().as_millis();
        #[cfg(target_arch = "wasm32")]
        0
    }
}

/// `s` cut to `MAX_ERROR` characters.
fn clip(s: &str) -> String {
    s.chars().take(MAX_ERROR).collect()
}

/// The `tools/call` answer for a tool's result. `structured`: the protocol version has
/// `structuredContent`.
fn tool_reply(result: &Result<Value, String>, structured: bool) -> Value {
    match result {
        Ok(v) => {
            let mut reply = json!({"content": [{"type": "text", "text": v.to_string()}], "isError": false});
            if structured {
                reply["structuredContent"] = v.clone();
            }
            reply
        }
        Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
    }
}

/// How a finished call is logged: what a send says (`sent`, `needsCode`...), else `ok` or `error`.
fn outcome_of(result: &Result<Value, String>) -> &str {
    match result {
        Ok(v) => v.get("outcome").and_then(Value::as_str).unwrap_or("ok"),
        Err(_) => "error",
    }
}

fn permissions_with_read(mut perms: BTreeSet<Permission>) -> BTreeSet<Permission> {
    perms.insert(Permission::Read);
    perms
}

impl Session {
    /// Answer one request from an AI app that came with `token`, at `now_ms` (Unix
    /// milliseconds, read by the app).
    pub fn mcp_handle(&mut self, token: &str, request: &Value, now_ms: i64) -> McpOutcome {
        self.drop_expired_pending(now_ms);
        let req = match protocol::parse_value(request) {
            Parsed::Invalid { id, code, message } => return McpOutcome::Reply(protocol::error(id.as_ref(), code, &message)),
            Parsed::Request(r) => r,
        };
        // A notification has no answer, whoever sent it.
        let Some(id) = req.id.clone() else { return McpOutcome::Reply(String::new()) };
        let Some(at) = self.mcp_token_at(token) else {
            if !self.mcp.bad_rate.allow(now_ms) {
                self.mcp.bad_skipped = self.mcp.bad_skipped.saturating_add(1);
                return McpOutcome::Reply(protocol::error(
                    Some(&id),
                    protocol::SLOW_DOWN,
                    "slow down: too many calls with a token that isn't accepted",
                ));
            }
            self.log_bad_token(now_ms);
            return McpOutcome::Reply(protocol::error(
                Some(&id),
                protocol::TOKEN_REFUSED,
                "this token isn't accepted: MCP is off in MailCraft, or the token was revoked",
            ));
        };
        let Some(record) = self.mcp.settings.tokens.get(at) else {
            return McpOutcome::Reply(protocol::error(Some(&id), protocol::INTERNAL_ERROR, "the token's record is missing"));
        };
        let (token_id, name, perms) = (record.id.clone(), record.name.clone(), record.permissions.clone());
        let allowed = self.mcp.rates.entry(token_id.clone()).or_default().allow(now_ms);
        if !allowed || self.mcp.busy.contains(&token_id) {
            log::info!(target: "mailcraft::mcp", "{name}: {} refused", req.method);
            return McpOutcome::Reply(protocol::error(Some(&id), protocol::SLOW_DOWN, "slow down: one call at a time, at most 10 a second"));
        }
        self.mcp_used(at, now_ms);
        let version = self.mcp.versions.get(&token_id).copied().unwrap_or(PROTOCOL_VERSIONS[0]);
        let structured = version == PROTOCOL_VERSIONS[0];
        match req.method.as_str() {
            "initialize" => {
                let asked = req.params.get("protocolVersion").and_then(Value::as_str);
                let version = protocol::negotiate(asked);
                self.mcp.versions.insert(token_id, version);
                McpOutcome::Reply(protocol::result(
                    &id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": {"name": "mailcraft", "title": "MailCraft", "version": env!("CARGO_PKG_VERSION")},
                        "instructions": "Message keys come from mail_list and mail_search. mail_read doesn't mark a message read. Changes made with mail_run can be undone in MailCraft.",
                    }),
                ))
            }
            "ping" => McpOutcome::Reply(protocol::result(&id, json!({}))),
            "tools/list" => McpOutcome::Reply(protocol::result(&id, json!({"tools": tools::listing(&perms, structured)}))),
            "tools/call" => self.mcp_call(&req.params, id, (&token_id, &name, &perms), structured, now_ms),
            _ => McpOutcome::Reply(protocol::error(Some(&id), protocol::METHOD_NOT_FOUND, "no such method")),
        }
    }

    /// Log a refused unknown token: one WARN a second, which says how many were refused since,
    /// so a flood of them can't push the real warnings out of the log.
    fn log_bad_token(&mut self, now_ms: i64) {
        let m = &mut self.mcp;
        if m.bad_logged.is_some_and(|t| now_ms.saturating_sub(t) < 1000 && t <= now_ms) {
            m.bad_skipped = m.bad_skipped.saturating_add(1);
            return;
        }
        m.bad_logged = Some(now_ms);
        let more = std::mem::take(&mut m.bad_skipped);
        log::warn!(target: "mailcraft::mcp", "a call was refused: unknown token, or MCP is off ({more} more refused)");
    }

    /// Whether `token_id` still has a token, with MCP on, that may use `tool`.
    fn mcp_still_allowed(&self, token_id: &str, tool: &str) -> bool {
        let Some(permission) = tools::find(tool).map(|t| t.permission) else { return false };
        self.mcp.settings.enabled
            && !self.mcp.unread
            && self.mcp.settings.tokens.iter().any(|t| t.id == token_id && t.permissions.contains(&permission))
    }

    /// The record that `token` is: every record's hash is compared, none stops the others.
    fn mcp_token_at(&self, token: &str) -> Option<usize> {
        let settings = &self.mcp.settings;
        let hash = tokens::hash_hex(token);
        let mut found = None;
        for (i, record) in settings.tokens.iter().enumerate() {
            if tokens::same(hash.as_bytes(), record.hash.as_bytes()) && found.is_none() {
                found = Some(i);
            }
        }
        found.filter(|_| settings.enabled && !self.mcp.unread)
    }

    /// Note that a token was used, at minute precision, saved when the minute changes.
    fn mcp_used(&mut self, at: usize, now_ms: i64) {
        let minute = now_ms.div_euclid(1000).div_euclid(60).saturating_mul(60);
        let Some(record) = self.mcp.settings.tokens.get_mut(at) else { return };
        if record.last_used != Some(minute) {
            record.last_used = Some(minute);
            if let Err(e) = self.save_mcp() {
                log::warn!(target: "mailcraft::mcp", "the settings couldn't be saved: {e}");
            }
        }
    }

    fn mcp_call(&mut self, params: &Value, id: Id, who: (&str, &str, &BTreeSet<Permission>), structured: bool, now_ms: i64) -> McpOutcome {
        let (token_id, name, perms) = who;
        let Some(tool_name) = params.get("name").and_then(Value::as_str) else {
            return McpOutcome::Reply(protocol::error(Some(&id), protocol::INVALID_PARAMS, "params.name must be the tool's name"));
        };
        let args = match params.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(a @ Value::Object(_)) => a.clone(),
            Some(_) => return McpOutcome::Reply(protocol::error(Some(&id), protocol::INVALID_PARAMS, "params.arguments must be an object")),
        };
        let Some(tool) = tools::find(tool_name) else {
            return McpOutcome::Reply(protocol::error(Some(&id), protocol::INVALID_PARAMS, "no such tool"));
        };
        if !perms.contains(&tool.permission) {
            log::info!(target: "mailcraft::mcp", "{name}: {} refused", tool.name);
            let denied = Err(format!("this token may not use {}", tool.name));
            return McpOutcome::Reply(protocol::result(&id, tool_reply(&denied, structured)));
        }
        let watch = Watch::start();
        // As `Session::run`: a panic in a tool is an error for the app, never a crash (the log
        // says where it happened).
        let step = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.mcp_tool(tool.name, &args, (perms, token_id), now_ms)))
            .unwrap_or_else(|_| Step::Done(Err("the tool failed unexpectedly".to_string())));
        match step {
            Step::Done(result) => {
                log::info!(target: "mailcraft::mcp", "{name}: {} {} in {} ms", tool.name, outcome_of(&result), watch.ms());
                McpOutcome::Reply(protocol::result(&id, tool_reply(&result, structured)))
            }
            Step::Refused(why) => {
                log::info!(target: "mailcraft::mcp", "{name}: {} refused in {} ms", tool.name, watch.ms());
                McpOutcome::Reply(protocol::result(&id, tool_reply(&Err(why), structured)))
            }
            Step::Later(kind) => {
                self.mcp.next_ticket = self.mcp.next_ticket.wrapping_add(1);
                let ticket = self.mcp.next_ticket;
                self.mcp.busy.insert(token_id.to_string());
                let wait = match kind {
                    Waiting::Sync { .. } => SYNC_WAIT_MS,
                    Waiting::Body { .. } => READ_WAIT_MS,
                    Waiting::Send { .. } => SEND_WAIT_MS,
                };
                log::info!(target: "mailcraft::mcp", "{name}: {} deferred", tool.name);
                self.mcp.waiting.push(Deferred {
                    ticket,
                    token_id: token_id.to_string(),
                    tool: tool.name,
                    started_ms: now_ms,
                    deadline_ms: now_ms.saturating_add(wait),
                    id,
                    structured,
                    kind,
                });
                McpOutcome::Later(ticket)
            }
        }
    }

    fn mcp_tool(&mut self, tool: &str, args: &Value, who: (&BTreeSet<Permission>, &str), now_ms: i64) -> Step {
        let (perms, token_id) = who;
        #[cfg(test)]
        if self.mcp.panic_in == Some(tool) {
            panic!("a tool forced to panic");
        }
        match tool {
            "mail_accounts" => self.mail_accounts(args).into(),
            "mail_list" => self.mail_list(args).into(),
            "mail_search" => self.mail_search(args).into(),
            "mail_read" => self.mail_read(args, false),
            "mail_commands" => self.mail_commands(args, perms).into(),
            "monitor_log" => self.monitor_log(args).into(),
            "mail_sync" => self.mail_sync(args),
            "mail_run" => self.mail_run(args).into(),
            "mail_send" => self.mail_send(args, token_id, now_ms),
            _ => Step::Done(Err("no such tool".to_string())),
        }
    }

    /// Calls that were answered `Later` and are done now, or out of time: the ticket and the
    /// line to answer with.
    pub fn mcp_finished(&mut self, now_ms: i64) -> Vec<(u64, String)> {
        self.drop_expired_pending(now_ms);
        let waiting = std::mem::take(&mut self.mcp.waiting);
        let mut done = Vec::new();
        for d in waiting {
            // A panic while deciding one call answers that call with an error: the others stay on
            // the list and the token is released on every path.
            let step = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if !self.mcp_still_allowed(&d.token_id, d.tool) {
                    Polled::Refused
                } else {
                    self.mcp_poll(&d, now_ms).map_or(Polled::Waiting, Polled::Answer)
                }
            }))
            .unwrap_or_else(|_| Polled::Answer(Err("the call failed unexpectedly".to_string())));
            match step {
                Polled::Refused => {
                    // Revoked, switched off or narrowed while it waited: no mail data goes out (a send
                    // that already went can't be undone, but its details aren't told).
                    self.mcp.busy.remove(&d.token_id);
                    log::info!(target: "mailcraft::mcp", "{} refused after {} ms: the token is no longer accepted", d.tool, now_ms.saturating_sub(d.started_ms).max(0));
                    let why = "this token isn't accepted: MCP is off in MailCraft, or the token was revoked";
                    done.push((d.ticket, protocol::error(Some(&d.id), protocol::TOKEN_REFUSED, why)));
                }
                Polled::Waiting => self.mcp.waiting.push(d),
                Polled::Answer(result) => {
                    self.mcp.busy.remove(&d.token_id);
                    let name = self.mcp.settings.tokens.iter().find(|t| t.id == d.token_id).map_or("a revoked app", |t| t.name.as_str());
                    log::info!(target: "mailcraft::mcp", "{name}: {} {} after {} ms", d.tool, outcome_of(&result), now_ms.saturating_sub(d.started_ms).max(0));
                    done.push((d.ticket, protocol::result(&d.id, tool_reply(&result, d.structured))));
                }
            }
        }
        done
    }

    /// The answer of a waiting call, if it has one.
    fn mcp_poll(&mut self, d: &Deferred, now_ms: i64) -> Option<Result<Value, String>> {
        #[cfg(test)]
        if self.mcp.panic_in == Some("poll") {
            panic!("a poll forced to panic");
        }
        let late = now_ms >= d.deadline_ms;
        match &d.kind {
            Waiting::Sync { accounts, failed } => {
                let running: Vec<&String> = accounts.iter().filter(|(_, done)| !**done).map(|(a, _)| a).collect();
                if !running.is_empty() && !late {
                    return None;
                }
                let done: Vec<&String> = accounts.iter().filter(|(a, done)| **done && !failed.contains_key(*a)).map(|(a, _)| a).collect();
                let failed: Vec<Value> = failed.iter().map(|(a, e)| json!({"account": a, "error": e})).collect();
                Some(Ok(json!({"done": done, "running": running, "failed": failed})))
            }
            Waiting::Body { args, outcome, .. } => match outcome {
                None if late => Some(Err("the message didn't download in time".to_string())),
                None => None,
                Some(Err(e)) => Some(Err(format!("the message couldn't be downloaded: {e}"))),
                Some(Ok(())) => Some(match self.mail_read(args, true) {
                    Step::Done(r) => r,
                    Step::Refused(why) => Err(why),
                    Step::Later(_) => Err("the message didn't download".to_string()),
                }),
            },
            Waiting::Send { compose_id, outcome } => match outcome {
                Some(Ok(SendEnd::Sent)) => Some(Ok(json!({"outcome": "sent", "id": compose_id}))),
                Some(Ok(SendEnd::Queued(why))) => Some(Ok(json!({"outcome": "queued", "id": compose_id, "error": why}))),
                Some(Err(why)) => Some(Err(format!("not sent: {why}"))),
                None if late => Some(Ok(json!({"outcome": "sending", "id": compose_id, "note": "handed to the server; no word yet"}))),
                None => None,
            },
        }
    }

    /// The socket gave up on a waiting call: forget it, and let its token call again.
    pub fn mcp_forget(&mut self, ticket: u64) {
        if let Some(at) = self.mcp.waiting.iter().position(|d| d.ticket == ticket) {
            let d = self.mcp.waiting.remove(at);
            self.mcp.busy.remove(&d.token_id);
        }
    }

    /// Whether a call waits for the network (the window then looks again soon, whatever else
    /// it draws).
    pub fn mcp_waiting(&self) -> bool {
        !self.mcp.waiting.is_empty()
    }

    /// What the network layer reported, noted for the calls that wait for it. Cheap: it copies
    /// keys and account ids only.
    pub(crate) fn mcp_saw(&mut self, e: &Event) {
        if self.mcp.waiting.is_empty() {
            return;
        }
        for d in &mut self.mcp.waiting {
            match (&mut d.kind, e) {
                (Waiting::Sync { accounts, failed }, Event::Status { account, status }) => {
                    let Some(done) = accounts.get_mut(account) else { continue };
                    match status {
                        SyncStatus::Online { .. } => *done = true,
                        SyncStatus::Error { error, .. } | SyncStatus::NeedsPassword { error } => {
                            *done = true;
                            failed.insert(account.clone(), clip(error));
                        }
                        SyncStatus::Offline => {
                            *done = true;
                            failed.insert(account.clone(), "it's offline".to_string());
                        }
                        _ => {}
                    }
                }
                (Waiting::Body { key, outcome, .. }, Event::Body { key: arrived, .. }) if key == arrived => *outcome = Some(Ok(())),
                (Waiting::Body { key, outcome, .. }, Event::BodyFailed { key: failed, error }) if key == failed => *outcome = Some(Err(clip(error))),
                (Waiting::Send { compose_id, outcome }, Event::Sent { id, .. }) if compose_id == id => *outcome = Some(Ok(SendEnd::Sent)),
                (Waiting::Send { compose_id, outcome }, Event::SendFailed { id, error, .. }) if compose_id == id => *outcome = Some(Err(clip(error))),
                (Waiting::Send { compose_id, outcome }, Event::Queued { id, error, .. }) if compose_id == id => {
                    *outcome = Some(Ok(SendEnd::Queued(clip(error))))
                }
                (Waiting::Send { compose_id, outcome }, Event::Protected { id, error: Some(error), .. }) if compose_id == id => {
                    *outcome = Some(Err(format!("it couldn't be signed or encrypted: {}", clip(error))));
                }
                _ => {}
            }
        }
    }

    /// Pending sends past their time are dropped.
    pub(super) fn drop_expired_pending(&mut self, now_ms: i64) {
        self.mcp.pending.retain(|p| p.expires_ms > now_ms);
    }

    /// Whether the approval is on but its secret isn't here yet (the app then asks the keychain
    /// for it at start).
    pub fn mcp_wants_secret(&self) -> bool {
        self.mcp.settings.approval.state == ApprovalState::Active && self.mcp.secret.is_none()
    }

    /// The keychain gave the approval secret (or none: approval then has to be set up again).
    pub(crate) fn mcp_secret_read(&mut self, what: &str, secret: Option<Secret>) {
        // Only while the app is still waiting for it: a late start-up read must not overwrite a
        // secret set up (or cleared) in the meantime.
        if what != APPROVAL_SECRET || !self.mcp_wants_secret() {
            return;
        }
        self.mcp.secret = secret.and_then(|s| approval::unbase32(&s.0));
    }

    /// The keychain kept the approval secret, or said it couldn't: approval is then off again.
    pub(crate) fn mcp_secret_kept(&mut self, what: &str, error: Option<String>) {
        let Some(why) = error.filter(|_| what == APPROVAL_SECRET) else { return };
        // Only a secret that was being kept (not one deleted by a reset) matters here.
        if self.mcp.settings.approval.state == ApprovalState::Active {
            self.mcp.settings.approval = Default::default();
            self.mcp.secret = None;
            self.mcp.pending.clear();
            if let Err(e) = self.save_mcp() {
                log::warn!(target: "mailcraft::mcp", "the settings couldn't be saved: {e}");
            }
            self.notice(format!("Approval couldn't be kept in the keychain: {why}"), true);
        }
    }

    /// Read `mcp.json` (at start). One that can't be read leaves MCP off this run and is never
    /// saved over.
    pub(crate) fn load_mcp(&mut self) {
        let Some(store) = self.store.clone() else { return };
        match store.load_doc::<McpSettings>(MCP_DOC) {
            Ok(mut settings) => {
                settings.version = 1;
                settings.tokens.retain(is_sound);
                settings.tokens.truncate(MAX_TOKENS);
                for t in &mut settings.tokens {
                    t.permissions.insert(Permission::Read);
                }
                settings.rules.daily_limit = settings.rules.daily_limit.min(200);
                self.mcp.settings = settings;
            }
            Err(e) => {
                log::warn!(target: "mailcraft::mcp", "the settings couldn't be read: {e}");
                self.mcp.unread = true;
                self.notice("Connected apps' settings (mcp.json) couldn't be read: they stay off until that's fixed", true);
            }
        }
    }

    /// Keep the settings in `mcp.json`: nothing when they couldn't be read, or came from a test.
    pub(crate) fn save_mcp(&mut self) -> Result<(), String> {
        let Some(store) = self.store.clone() else { return Ok(()) };
        if self.mcp.unread || self.mcp.memory_only {
            return Ok(());
        }
        self.mcp.settings.version = 1;
        store.save_doc(MCP_DOC, &self.mcp.settings).map_err(|e| e.to_string())
    }

    /// Add a token in memory and turn MCP on, never saving: for tests and the fuzz target.
    #[doc(hidden)]
    pub fn mcp_seed_token(&mut self, token: &str, perms: &[Permission]) {
        let hash = tokens::hash_hex(token);
        let permissions = permissions_with_read(perms.iter().copied().collect());
        self.mcp.settings.tokens.push(TokenRecord {
            id: hash.chars().take(12).collect(),
            name: "test".into(),
            hash,
            permissions,
            created: 0,
            last_used: None,
        });
        self.mcp.settings.enabled = true;
        self.mcp.memory_only = true;
    }

    /// Why the owner's commands can't change the settings: they weren't read (the card says so).
    pub fn mcp_unusable(&self) -> Option<&'static str> {
        self.mcp.unread.then_some("the connected apps' settings couldn't be read")
    }
}

/// Whether a record read from `mcp.json` is one that can have been made here.
fn is_sound(t: &TokenRecord) -> bool {
    t.hash.len() == 64 && t.hash.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()) && t.id.len() == 12 && t.hash.starts_with(&t.id)
}

/// The arguments of a tool that takes none.
fn no_arguments(args: &Value) -> Result<(), String> {
    Args::new(args, &[]).map(|_| ())
}
