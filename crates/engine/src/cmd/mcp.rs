//! The commands that set up MCP (File › Options › Connected apps). Every one runs only from a
//! click in that card (`Session::mcp.owner_ui`): `--cmd`, `mailcraft-cli run`, `mail_run` and
//! scripts can't reach them, so a program can't hand itself a token.

use std::collections::BTreeSet;

use super::*;
use crate::mcp::approval::{self, ApprovalRecord, ApprovalState};
use crate::mcp::tokens::{self, TokenRecord};
use crate::mcp::{APPROVAL_SECRET, MAX_TOKENS, Permission};
use mailcraft_model::Job;

/// Only a click in the Connected apps card.
fn owner_only(s: &Session) -> Option<&'static str> {
    (!s.mcp.owner_ui).then_some("only from File › Options › Connected apps")
}

const LOCATION: &str = "File › Options";
/// The longest name a token has, characters.
const MAX_NAME: usize = 64;
/// Tries at a token whose id another has (12 characters of a hash: all but never).
const DRAWS: usize = 3;

pub(super) fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("mcp.setEnabled", "Allow connected apps", LOCATION, set_enabled).params(r#"{"on": bool}"#).when(owner_only),
        CommandSpec::new("mcp.createToken", "Make a token for an app", LOCATION, create_token)
            .params(r#"{"name": string, "permissions": ["read"|"organize"|"send"]}"#)
            .when(owner_only),
        CommandSpec::new("mcp.dismissToken", "Done with the new token", LOCATION, dismiss_token).when(owner_only),
        CommandSpec::new("mcp.revokeToken", "Revoke a token", LOCATION, revoke_token).params(r#"{"id": string}"#).when(owner_only),
        CommandSpec::new("mcp.setupApproval", "Set up approval codes", LOCATION, setup_approval).when(owner_only),
        CommandSpec::new("mcp.confirmApproval", "Confirm an approval code", LOCATION, confirm_approval)
            .params(r#"{"code": string}"#)
            .when(owner_only),
        CommandSpec::new("mcp.resetApproval", "Reset approval codes", LOCATION, reset_approval).when(owner_only),
        CommandSpec::new("mcp.unlockApproval", "Unlock sending", LOCATION, unlock_approval).when(owner_only),
        CommandSpec::new("mcp.setRules", "Set the rules for sending without a code", LOCATION, set_rules)
            .params(r#"{"noCodeReplies"?: bool, "dailyLimit"?: number (0 to 200)}"#)
            .when(owner_only),
        CommandSpec::new("mcp.sendPending", "Send a message held for approval", LOCATION, send_pending).params(r#"{"id": string}"#).when(owner_only),
        CommandSpec::new("mcp.discardPending", "Discard a message held for approval", LOCATION, discard_pending)
            .params(r#"{"id": string}"#)
            .when(owner_only),
    ]
}

/// The settings can be changed: they were read.
fn writable(s: &Session) -> Result<(), CmdError> {
    s.mcp_unusable().map_or(Ok(()), |why| Err(CmdError::Disabled(why.to_string())))
}

fn set_enabled(s: &mut Session, v: &Value) -> CmdResult {
    writable(s)?;
    let on = p::bool(v, "on").ok_or_else(|| CmdError::Params("`on` (bool) is required".into()))?;
    if on && s.mcp.settings.tokens.is_empty() {
        return Err(CmdError::Params("make a token first".into()));
    }
    s.mcp.settings.enabled = on;
    s.save_mcp().map_err(CmdError::Failed)?;
    Ok(json!({"on": on}))
}

/// A token's name as kept: control characters gone, trimmed, 1 to 64 characters.
fn clean_name(name: &str) -> Result<String, CmdError> {
    let name: String = name.chars().filter(|c| !c.is_control()).collect();
    let name = name.trim().to_string();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return Err(CmdError::Params(format!("`name` must be from 1 to {MAX_NAME} characters")));
    }
    Ok(name)
}

fn permissions_param(v: &Value) -> Result<BTreeSet<Permission>, CmdError> {
    let mut perms = BTreeSet::from([Permission::Read]);
    match v.get("permissions") {
        None | Some(Value::Null) => {}
        Some(Value::Array(list)) => {
            for item in list {
                let code = item.as_str().ok_or_else(|| CmdError::Params("`permissions` holds strings".into()))?;
                perms.insert(Permission::from_code(code).ok_or_else(|| CmdError::Params("a permission is read, organize or send".into()))?);
            }
        }
        Some(_) => return Err(CmdError::Params("`permissions` must be a list".into())),
    }
    Ok(perms)
}

fn create_token(s: &mut Session, v: &Value) -> CmdResult {
    writable(s)?;
    let name = clean_name(p::req_str(v, "name")?)?;
    let permissions = permissions_param(v)?;
    let random = s.mcp.random.ok_or_else(|| CmdError::Disabled("no secure random source".into()))?;
    if s.mcp.settings.tokens.len() >= MAX_TOKENS {
        return Err(CmdError::Params(format!("there are {MAX_TOKENS} tokens already: revoke one first")));
    }
    for _ in 0..DRAWS {
        let token = random(32).and_then(|bytes| tokens::make_token(&bytes)).ok_or_else(|| CmdError::Failed("no secure random source".into()))?;
        let hash = tokens::hash_hex(&token);
        let id: String = hash.chars().take(12).collect();
        if s.mcp.settings.tokens.iter().any(|t| t.id == id) {
            continue;
        }
        s.mcp.settings.tokens.push(TokenRecord { id: id.clone(), name: name.clone(), hash, permissions, created: s.clock.now, last_used: None });
        if let Err(e) = s.save_mcp() {
            s.mcp.settings.tokens.pop();
            return Err(CmdError::Failed(format!("the token couldn't be kept: {e}")));
        }
        s.mcp.revealed = Some(crate::session::Revealed { name, token });
        return Ok(json!({"id": id}));
    }
    Err(CmdError::Failed("a token couldn't be made: try again".into()))
}

fn dismiss_token(s: &mut Session, _: &Value) -> CmdResult {
    s.mcp.revealed = None;
    Ok(json!({}))
}

fn revoke_token(s: &mut Session, v: &Value) -> CmdResult {
    writable(s)?;
    let id = p::req_str(v, "id")?;
    let before = s.mcp.settings.tokens.len();
    s.mcp.settings.tokens.retain(|t| t.id != id);
    let revoked = s.mcp.settings.tokens.len() < before;
    if revoked {
        s.save_mcp().map_err(CmdError::Failed)?;
    }
    Ok(json!({"revoked": revoked}))
}

/// The secret that codes are made from: 20 random bytes, shown as a QR code and as text until a
/// code made from it confirms it.
fn setup_approval(s: &mut Session, _: &Value) -> CmdResult {
    writable(s)?;
    if s.mcp.settings.approval.state == ApprovalState::Active && s.mcp.secret.is_some() {
        return Err(CmdError::Params("approval is on already: reset it first".into()));
    }
    let random = s.mcp.random.ok_or_else(|| CmdError::Disabled("no secure random source".into()))?;
    let bytes = random(20).filter(|b| b.len() == 20).ok_or_else(|| CmdError::Failed("no secure random source".into()))?;
    let secret_b32 = approval::base32(&bytes);
    let uri = approval::otpauth_uri(&secret_b32);
    s.mcp.setup = Some(crate::session::ApprovalSetup { secret_b32, uri, bytes });
    Ok(json!({}))
}

/// A code from the authenticator app that was set up: approval is on, and the secret is kept in
/// the keychain.
fn confirm_approval(s: &mut Session, v: &Value) -> CmdResult {
    writable(s)?;
    let code = approval::parse_code(p::req_str(v, "code")?).ok_or_else(|| CmdError::Params("the code is six digits".into()))?;
    let setup = s.mcp.setup.clone().ok_or_else(|| CmdError::Disabled("set up approval first".into()))?;
    // A scratch record: a wrong code here counts for nothing (only the owner can be typing it).
    let mut scratch = ApprovalRecord { state: ApprovalState::Active, ..Default::default() };
    let approval::Verdict::Ok { step } = approval::check(&mut scratch, &setup.bytes, code, s.clock.now) else {
        return Err(CmdError::Params("that code isn't right: try the next one".into()));
    };
    s.mcp.secret = Some(setup.bytes);
    s.mcp.settings.approval = ApprovalRecord { state: ApprovalState::Active, last_step: step, failures: 0, locked: false };
    s.mcp.setup = None;
    s.save_mcp().map_err(CmdError::Failed)?;
    s.push_job(Job::KeepAppSecret { what: APPROVAL_SECRET.to_string(), secret: Some(mailcraft_model::job::Secret(setup.secret_b32)) });
    Ok(json!({"active": true}))
}

/// Approval off, its secret let go of, and every held send discarded: for a lost phone.
fn reset_approval(s: &mut Session, _: &Value) -> CmdResult {
    writable(s)?;
    s.mcp.settings.approval = ApprovalRecord::default();
    s.mcp.secret = None;
    s.mcp.setup = None;
    s.mcp_discard_pending(None);
    s.save_mcp().map_err(CmdError::Failed)?;
    s.push_job(Job::KeepAppSecret { what: APPROVAL_SECRET.to_string(), secret: None });
    log::warn!(target: "mailcraft::mcp", "approval reset");
    Ok(json!({}))
}

fn unlock_approval(s: &mut Session, _: &Value) -> CmdResult {
    writable(s)?;
    s.mcp.settings.approval.locked = false;
    s.mcp.settings.approval.failures = 0;
    s.save_mcp().map_err(CmdError::Failed)?;
    log::warn!(target: "mailcraft::mcp", "sending unlocked");
    Ok(json!({}))
}

/// The most automatic sends a day that can be set.
const MAX_DAILY: i64 = 200;

fn set_rules(s: &mut Session, v: &Value) -> CmdResult {
    writable(s)?;
    // Everything given is read before anything changes.
    let no_code = match v.get("noCodeReplies") {
        None => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => return Err(CmdError::Params("`noCodeReplies` must be true or false".into())),
    };
    let limit = match v.get("dailyLimit") {
        None => None,
        Some(n) => match n.as_i64().filter(|n| (0..=MAX_DAILY).contains(n)).and_then(|n| u32::try_from(n).ok()) {
            Some(n) => Some(n),
            None => return Err(CmdError::Params(format!("`dailyLimit` must be a whole number from 0 to {MAX_DAILY}"))),
        },
    };
    let rules = &mut s.mcp.settings.rules;
    rules.no_code_replies = no_code.unwrap_or(rules.no_code_replies);
    rules.daily_limit = limit.unwrap_or(rules.daily_limit);
    s.save_mcp().map_err(CmdError::Failed)?;
    Ok(json!({"noCodeReplies": s.mcp.settings.rules.no_code_replies, "dailyLimit": s.mcp.settings.rules.daily_limit}))
}

fn send_pending(s: &mut Session, v: &Value) -> CmdResult {
    s.mcp_send_pending(p::req_str(v, "id")?)
}

fn discard_pending(s: &mut Session, v: &Value) -> CmdResult {
    let id = p::req_str(v, "id")?;
    Ok(json!({"discarded": s.mcp_discard_pending(Some(id))}))
}
