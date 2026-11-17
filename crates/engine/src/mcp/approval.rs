//! The authenticator code that approves a send (RFC 6238 over RFC 4226: HMAC-SHA1, 6 digits,
//! 30 seconds), and the record kept of it. Pure: the secret is read from the keychain and held
//! by the session; nothing here keeps, prints or logs it.

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha1::Sha1;

use super::LOCK_AFTER;
use super::tokens::same;

/// The most bytes a secret decodes to.
const MAX_SECRET: usize = 64;
/// Seconds a code lives.
const PERIOD: i64 = 30;
const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// RFC 4226's code for `counter`: HMAC-SHA1, dynamic truncation, six digits.
pub fn hotp(secret: &[u8], counter: u64) -> u32 {
    let Ok(mut mac) = <Hmac<Sha1> as KeyInit>::new_from_slice(secret) else { return 0 };
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let Some(&last) = digest.last() else { return 0 };
    let at = usize::from(last & 0x0f);
    let word = digest.get(at..at + 4).and_then(|b| <[u8; 4]>::try_from(b).ok()).map_or(0, u32::from_be_bytes);
    (word & 0x7fff_ffff) % 1_000_000
}

/// The time step of `now_s` (Unix seconds).
pub fn step(now_s: i64) -> i64 {
    now_s.div_euclid(PERIOD)
}

/// The code at `step`; `None` before 1970.
pub fn code_at(secret: &[u8], step: i64) -> Option<u32> {
    u64::try_from(step).ok().map(|s| hotp(secret, s))
}

/// RFC 4648 base32, upper case, no padding.
pub fn base32(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let (mut bits, mut have) = (0u32, 0u32);
    for b in bytes {
        bits = (bits << 8) | u32::from(*b);
        have += 8;
        while have >= 5 {
            have -= 5;
            out.push(char::from(ALPHABET.get(((bits >> have) & 31) as usize).copied().unwrap_or(b'A')));
        }
        bits &= (1 << have) - 1;
    }
    if have > 0 {
        out.push(char::from(ALPHABET.get(((bits << (5 - have)) & 31) as usize).copied().unwrap_or(b'A')));
    }
    out
}

/// Bytes from base32 text: either case, spaces and `=` ignored; `None` for any other character
/// or for more than 64 bytes.
pub fn unbase32(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut bits, mut have) = (0u32, 0u32);
    for c in text.chars().filter(|c| *c != ' ' && *c != '=') {
        let upper = u8::try_from(c.to_ascii_uppercase()).ok()?;
        let v = u32::try_from(ALPHABET.iter().position(|a| *a == upper)?).ok()?;
        bits = (bits << 5) | v;
        have += 5;
        if have >= 8 {
            have -= 8;
            out.push(u8::try_from((bits >> have) & 0xff).ok()?);
            if out.len() > MAX_SECRET {
                return None;
            }
            bits &= (1 << have) - 1;
        }
    }
    Some(out)
}

/// What an authenticator app scans.
pub fn otpauth_uri(secret_b32: &str) -> String {
    format!("otpauth://totp/MailCraft?secret={secret_b32}&issuer=MailCraft")
}

/// A typed code: exactly six ASCII digits, spaces around it left out.
pub fn parse_code(text: &str) -> Option<u32> {
    let t = text.trim();
    (t.len() == 6 && t.bytes().all(|b| b.is_ascii_digit())).then(|| t.parse().ok()).flatten()
}

/// What of the approval is kept in `mcp.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ApprovalRecord {
    pub state: ApprovalState,
    /// The last time step a code was taken at: that step and any before it are never taken again.
    pub last_step: i64,
    /// Wrong codes since the last right one.
    pub failures: u32,
    pub locked: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalState {
    #[default]
    Off,
    Active,
}

/// How a code came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Ok { step: i64 },
    Wrong,
    Replayed,
    Locked,
    NotSetUp,
}

/// A code's six digits as ASCII, for comparing.
fn digits(code: u32) -> [u8; 6] {
    let mut out = [b'0'; 6];
    let mut n = code % 1_000_000;
    for slot in out.iter_mut().rev() {
        *slot = b'0' + u8::try_from(n % 10).unwrap_or(0);
        n /= 10;
    }
    out
}

/// Check `code` at `now_s`: the code of the step before, of this one and of the next are taken;
/// a step at or before `last_step` never is. A wrong or repeated code counts as a failure, and
/// `LOCK_AFTER` of them lock the record (a locked one answers without looking at the code).
pub fn check(rec: &mut ApprovalRecord, secret: &[u8], code: u32, now_s: i64) -> Verdict {
    if rec.locked {
        return Verdict::Locked;
    }
    if rec.state != ApprovalState::Active {
        return Verdict::NotSetUp;
    }
    let now = step(now_s);
    let sent = digits(code);
    // Every candidate is compared, whichever matches.
    let mut matching = Vec::new();
    for at in [now.saturating_sub(1), now, now.saturating_add(1)] {
        let theirs = code_at(secret, at).map(digits);
        let hit = theirs.is_some_and(|t| same(&t, &sent));
        if hit {
            matching.push(at);
        }
    }
    if let Some(step) = matching.iter().copied().filter(|s| *s > rec.last_step).max() {
        rec.last_step = step;
        rec.failures = 0;
        return Verdict::Ok { step };
    }
    rec.failures = rec.failures.saturating_add(1);
    if rec.failures >= LOCK_AFTER {
        rec.locked = true;
    }
    if matching.is_empty() { Verdict::Wrong } else { Verdict::Replayed }
}
