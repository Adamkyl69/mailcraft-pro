//! Tokens, and the settings kept in `mcp.json`.
//!
//! A token is shown once, when it's made: only its SHA-256 is kept, and a call's token is
//! compared with every record's hash in constant time.

use std::collections::{BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{CALLS_PER_SECOND, DAILY_LIMIT, Permission};

/// What every token starts with.
pub const PREFIX: &str = "mcp_";
/// Bytes of randomness in a token.
const RANDOM_BYTES: usize = 32;
/// Length of a whole token: the prefix and 32 bytes in base64url without padding.
const TOKEN_LEN: usize = 4 + 43;

/// Base64url, no padding.
pub fn b64url(bytes: &[u8]) -> String {
    mailcraft_model::base64::encode(bytes).replace('+', "-").replace('/', "_").trim_end_matches('=').to_string()
}

/// A token from exactly 32 random bytes; `None` for any other number.
pub fn make_token(random32: &[u8]) -> Option<String> {
    (random32.len() == RANDOM_BYTES).then(|| format!("{PREFIX}{}", b64url(random32)))
}

/// Whether `t` is shaped like a token (it doesn't say it's one that's accepted).
pub fn looks_like_token(t: &str) -> bool {
    t.len() == TOKEN_LEN && t.strip_prefix(PREFIX).is_some_and(|rest| rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
}

/// The SHA-256 of a token's text, in lower-case hex.
pub fn hash_hex(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether two byte strings are the same, without stopping at the first difference.
pub fn same(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut differing = 0u8;
    for (x, y) in a.iter().zip(b) {
        differing |= x ^ y;
    }
    differing == 0
}

/// One app's token, as kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenRecord {
    /// The first 12 characters of the hash: names it for revoking, never authenticates.
    pub id: String,
    /// Trimmed, 1 to 64 characters, no control characters.
    pub name: String,
    /// 64 hex characters.
    pub hash: String,
    /// Always holds `Read`.
    pub permissions: BTreeSet<Permission>,
    /// Unix seconds.
    pub created: i64,
    /// Unix seconds, at minute precision.
    pub last_used: Option<i64>,
}

/// The rules for sending without a code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SendRules {
    pub no_code_replies: bool,
    pub daily_limit: u32,
}

impl Default for SendRules {
    fn default() -> Self {
        SendRules { no_code_replies: true, daily_limit: DAILY_LIMIT }
    }
}

/// `mcp.json`: everything about MCP that's kept.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct McpSettings {
    pub version: u32,
    pub enabled: bool,
    pub tokens: Vec<TokenRecord>,
    pub rules: SendRules,
    /// The local day number and how many automatic sends that day made.
    pub sent_today: (i64, u32),
}

/// The times of a token's last calls (milliseconds), for at most 10 a second.
#[derive(Debug, Default)]
pub struct RateWindow {
    calls: VecDeque<i64>,
}

impl RateWindow {
    /// Whether a call at `now_ms` is within the limit; one that is is counted.
    pub fn allow(&mut self, now_ms: i64) -> bool {
        while self.calls.front().is_some_and(|t| now_ms.saturating_sub(*t) >= 1000 || *t > now_ms) {
            self.calls.pop_front();
        }
        if self.calls.len() >= CALLS_PER_SECOND {
            return false;
        }
        self.calls.push_back(now_ms);
        true
    }
}
