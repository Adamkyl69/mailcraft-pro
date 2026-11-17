//! The commands that set up MCP (File › Options › Connected apps). Every one runs only from a
//! click in that card (`Session::mcp.owner_ui`): `--cmd`, `mailcraft-cli run`, `mail_run` and
//! scripts can't reach them, so a program can't hand itself a token.

use std::collections::BTreeSet;

use super::*;
use crate::mcp::tokens::{self, TokenRecord};
use crate::mcp::{MAX_TOKENS, Permission};

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
