//! MCP: what local AI apps (Claude Desktop, Claude Code, any MCP client on this computer) ask of
//! the running MailCraft, and how the engine answers (`docs/specs/mcp-server-build.md`).
//!
//! This module is the **pure** part: JSON-RPC lines, tokens and the persisted settings, the tool
//! table. It knows nothing of the `Session`; the part that acts on it is `session/mcp.rs`, which
//! uses these (one way: `session` to `mcp`, so the two never form a cycle with `cmd`).

pub mod approval;
pub mod protocol;
pub mod rules;
pub mod tokens;
pub mod tools;

#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

/// The protocol versions spoken, newest first.
pub const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
/// A JSON-RPC line, bytes.
pub const MAX_LINE: usize = 1 << 20;
/// JSON nesting.
pub const MAX_DEPTH: usize = 64;
/// Any string argument but `body`.
pub const MAX_STRING: usize = 64 << 10;
/// `mail_send` body.
pub const MAX_BODY: usize = 512 << 10;
/// `limit`, keys, ...
pub const MAX_LIST: usize = 200;
/// `monitor_log` lines.
pub const MAX_LOG: usize = 500;
/// `mail_read` text, characters.
pub const MAX_TEXT: usize = 200_000;
/// `mail_read` raw header block, bytes.
pub const MAX_HEADERS: usize = 64 << 10;
/// To and Cc together.
pub const MAX_RECIPIENTS: usize = 10;
/// Tokens that can exist at once.
pub const MAX_TOKENS: usize = 20;
/// Calls a token may make in one second.
pub const CALLS_PER_SECOND: usize = 10;
/// How long `mail_sync` waits for the accounts, in milliseconds.
pub const SYNC_WAIT_MS: i64 = 120_000;
/// How long `mail_read` waits for a body to download.
pub const READ_WAIT_MS: i64 = 60_000;
/// How long a send waits for the server's word.
pub const SEND_WAIT_MS: i64 = 60_000;
/// How long a held send waits for its code.
pub const PENDING_MS: i64 = 600_000;
/// Sends held at once.
pub const MAX_PENDING: usize = 5;
/// Automatic sends a day, by default (settable 0 to 200).
pub const DAILY_LIMIT: u32 = 20;
/// Wrong codes before sending is locked.
pub const LOCK_AFTER: u32 = 5;
/// The keychain entry's `what` for the approval secret.
pub const APPROVAL_SECRET: &str = "mcp-approval";

/// What a token may do. `Read` is always held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Permission {
    Read,
    Organize,
    Send,
}

impl Permission {
    /// `read`, `organize`, `send`.
    pub fn code(self) -> &'static str {
        match self {
            Permission::Read => "read",
            Permission::Organize => "organize",
            Permission::Send => "send",
        }
    }

    pub fn from_code(code: &str) -> Option<Permission> {
        [Permission::Read, Permission::Organize, Permission::Send].into_iter().find(|p| p.code() == code)
    }
}

/// A line of this run's log, as the app's log keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LogLine {
    /// Unix seconds, UTC.
    pub at: i64,
    /// `WARN` or `ERROR`.
    pub level: String,
    pub target: String,
    pub text: String,
    pub panic: bool,
}

/// What `monitor_log` asks of the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogQuery {
    pub limit: usize,
    pub since: Option<i64>,
    pub errors_only: bool,
}

/// How an AI app starts the bridge, for the Settings card's snippets (set by the app). An
/// argument that holds [`TOKEN_PLACEHOLDER`] gets the token there: a Flatpak's `flatpak run`
/// doesn't pass the caller's environment on, so it is given as `--env=MAILCRAFT_MCP_TOKEN=…`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BridgeCommand {
    pub program: String,
    pub args: Vec<String>,
}

/// Where in a [`BridgeCommand`]'s arguments the snippets put the new token.
pub const TOKEN_PLACEHOLDER: &str = "{MAILCRAFT_MCP_TOKEN}";

/// The argument that gives a sandboxed bridge the token (`flatpak run … --env=…`).
pub const TOKEN_PLACEHOLDER_ARG: &str = "--env=MAILCRAFT_MCP_TOKEN={MAILCRAFT_MCP_TOKEN}";

impl BridgeCommand {
    /// The arguments with `token` where [`TOKEN_PLACEHOLDER`] is.
    pub fn args_with_token(&self, token: &str) -> Vec<String> {
        self.args.iter().map(|a| a.replace(TOKEN_PLACEHOLDER, token)).collect()
    }
}
