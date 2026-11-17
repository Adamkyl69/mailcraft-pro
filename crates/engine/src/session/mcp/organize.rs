//! MCP's organize tools: fetch new mail, and run a few easily undone commands on messages. The
//! commands run aside, on a selection of their own, so what the user has open is left as it is.

use super::*;
use crate::mcp::{MAX_LIST, MAX_STRING, tools::RUNNABLE};

/// The most keys `mail_run` acts on.
const MAX_KEYS: usize = 100;

impl Session {
    pub(super) fn mail_sync(&mut self, args: &Value) -> Step {
        match self.sync_accounts(args) {
            Ok(accounts) => Step::Later(Waiting::Sync { accounts, failed: BTreeMap::new() }),
            Err(e) => Step::Done(Err(e)),
        }
    }

    /// Ask the servers, and the accounts asked.
    fn sync_accounts(&mut self, args: &Value) -> Result<BTreeMap<String, bool>, String> {
        let a = Args::new(args, &["account"])?;
        let only = a.str("account", MAX_STRING)?;
        if self.prefs.work_offline {
            return Err("MailCraft is working offline".to_string());
        }
        if let Some(id) = only {
            let account = self.account(id).ok_or_else(|| format!("no account {id}"))?;
            if account.is_local() {
                return Err("the sample mailbox has no server".to_string());
            }
        }
        let ids: Vec<String> = self
            .accounts
            .iter()
            .filter(|a| a.config.has_server() && only.map_or(!a.config.check_by_hand, |id| a.config.id == id))
            .map(|a| a.config.id.clone())
            .collect();
        if ids.is_empty() {
            return Err("there's no account with a server to fetch mail from".to_string());
        }
        self.sync_all(only);
        Ok(ids.into_iter().map(|id| (id, false)).collect())
    }

    pub(super) fn mail_run(&mut self, args: &Value) -> Result<Value, String> {
        let a = Args::new(args, &["command", "keys", "params"])?;
        let command = a.req_str("command", MAX_STRING)?;
        if !RUNNABLE.contains(&command) {
            return Err(format!("{command} isn't one an AI app may run"));
        }
        let texts = a.strings("keys", 1, MAX_KEYS.min(MAX_LIST))?.ok_or_else(|| "`keys` is required".to_string())?;
        let params = a.object("params")?.cloned().unwrap_or_else(|| json!({}));
        let keys: Vec<MessageKey> = texts
            .iter()
            .map(|t| MessageKey::from_id(t).ok_or_else(|| "`keys` holds something that isn't a message key".to_string()))
            .collect::<Result<_, _>>()?;
        let Some(first) = keys.first() else { return Err("`keys` is required".to_string()) };
        if keys.iter().any(|k| k.account != first.account || k.folder != first.folder) {
            return Err("keys from one folder at a time".to_string());
        }
        let (account, folder) = (first.account.clone(), first.folder.clone());
        self.narrow_command(command, &params, &account)?;
        let ran = self.aside(true, |s| {
            s.open_view(View::Folder { account, folder });
            let present: Vec<MessageKey> = keys.iter().filter(|k| s.list.iter().any(|m| m.key == **k)).cloned().collect();
            let missing = keys.len().saturating_sub(present.len());
            if missing > 0 {
                return Err(format!("{missing} of the keys aren't in that folder: nothing was run"));
            }
            s.focus = present.first().cloned();
            s.anchor = s.focus.clone();
            s.selection = present;
            s.run(command, &params).map_err(|e| match e {
                CmdError::Params(m) | CmdError::Disabled(m) | CmdError::Failed(m) => m,
                CmdError::Unknown(_) => format!("{command} isn't one an AI app may run"),
            })
        })?;
        ran.map(|result| json!({"result": result}))
    }

    /// What `mail_run` refuses of the commands it allows: a move to another account or to a
    /// folder where mail goes to be deleted, and a category that isn't in the list.
    fn narrow_command(&self, command: &str, params: &Value, account: &str) -> Result<(), String> {
        match command {
            "mail.move" => {
                if params.get("account").is_some() {
                    return Err("mail.move across accounts isn't allowed".to_string());
                }
                let folder = params.get("folder").and_then(Value::as_str).ok_or_else(|| "`params.folder` is required".to_string())?;
                let role = match folder.strip_prefix("role:") {
                    Some(code) => FolderRole::from_code(code),
                    None => self.folder(account, folder).map(|f| f.role),
                };
                if matches!(role, Some(FolderRole::Trash | FolderRole::Junk)) {
                    return Err("an AI app may not move mail to Trash or Junk".to_string());
                }
            }
            "mail.categorize" => {
                if params.get("keyword").is_some() {
                    return Err("mail.categorize takes a category from the list, not a keyword".to_string());
                }
                let name = params.get("category").and_then(Value::as_str).ok_or_else(|| "`params.category` is required".to_string())?;
                if self.category(name).is_none() {
                    return Err(format!("no category called {name}"));
                }
            }
            _ => {}
        }
        Ok(())
    }
}
