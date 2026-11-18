//! MCP's read tools: accounts, lists, search, a message's text, the log. None of them changes
//! what the user sees: searching runs aside, and a message is read without being shown or marked.

use super::*;
use crate::mcp::{LogQuery, MAX_HEADERS, MAX_LIST, MAX_LOG, MAX_STRING, MAX_TEXT, tools::RUNNABLE};

/// The longest search query.
const MAX_QUERY: usize = 1024;

impl Session {
    /// A message in a list, as an AI app sees it.
    fn mcp_msg(&self, m: &Summary) -> Value {
        let mut j = crate::cmd::summary_json(m);
        j["to"] = json!(mailcraft_model::addr::format_list(&m.to));
        j["answered"] = json!(m.flags.answered);
        j["forwarded"] = json!(m.flags.forwarded);
        j["pinned"] = json!(m.flags.has(mailcraft_model::PINNED));
        j["categories"] = json!(self.categories_of(m).into_iter().map(|c| c.name).collect::<Vec<_>>());
        j
    }

    pub(super) fn mail_accounts(&mut self, args: &Value) -> Result<Value, String> {
        no_arguments(args)?;
        let listed = self.run("account.list", &json!({})).map_err(|e| e.to_string())?;
        let accounts: Vec<Value> = listed
            .as_array()
            .map(|list| {
                list.iter()
                    .map(|a| {
                        json!({
                            "account": a["account"],
                            "email": a["email"],
                            "name": a["name"],
                            "addresses": a["addresses"].as_array().map(|l| l.iter().map(|i| json!({"address": i["address"], "name": i["name"]})).collect::<Vec<_>>()).unwrap_or_default(),
                            "status": a["status"],
                            "folders": a["folders"],
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(json!({"accounts": accounts}))
    }

    pub(super) fn mail_list(&mut self, args: &Value) -> Result<Value, String> {
        let a = Args::new(args, &["account", "folder", "limit", "offset", "unread_only"])?;
        let account = a.req_str("account", MAX_STRING)?;
        let folder = a.req_str("folder", MAX_STRING)?;
        let limit = usize::try_from(a.int("limit", 1, MAX_LIST as i64)?.unwrap_or(50)).unwrap_or(50);
        let offset = usize::try_from(a.int("offset", 0, 1_000_000)?.unwrap_or(0)).unwrap_or(0);
        let unread_only = a.bool("unread_only")?.unwrap_or(false);
        if self.account(account).is_none() {
            return Err(format!("no account {account}"));
        }
        if self.folder(account, folder).is_none() {
            return Err(format!("no folder {folder} in {account}"));
        }
        // A folder that can't be read just now (being compacted, its marks being written) is
        // said to be busy rather than empty.
        if let Err(e) = self.load_cached(account, folder) {
            if mailboxes::read_soon(&e) {
                return Ok(json!({"total": 0, "offset": offset, "busy": true, "messages": []}));
            }
            return Err("that folder couldn't be read".to_string());
        }
        let kept: Vec<SharedSummary> =
            self.folder_state(account, folder).messages.iter().filter(|m| !m.flags.deleted && (!unread_only || !m.flags.seen)).cloned().collect();
        let messages: Vec<Value> = kept.iter().skip(offset).take(limit).map(|m| self.mcp_msg(m)).collect();
        Ok(json!({"total": kept.len(), "offset": offset, "messages": messages}))
    }

    pub(super) fn mail_search(&mut self, args: &Value) -> Result<Value, String> {
        let a = Args::new(args, &["query", "limit"])?;
        let query = a.req_str("query", MAX_QUERY)?.to_string();
        let limit = usize::try_from(a.int("limit", 1, MAX_LIST as i64)?.unwrap_or(50)).unwrap_or(50);
        if query.trim().is_empty() {
            return Err("`query` is empty".to_string());
        }
        let found = self.aside(false, |s| {
            s.open_view(View::Unified { role: FolderRole::Inbox });
            s.run_search_with(&query, SearchScope::All, false);
            let shown: Vec<SharedSummary> = s.list.iter().take(limit).cloned().collect();
            (s.list.len(), shown.iter().map(|m| s.mcp_msg(m)).collect::<Vec<Value>>())
        })?;
        Ok(json!({"found": found.0, "messages": found.1}))
    }

    /// A message's text and sender. Its source isn't here yet: it's asked for, and the call
    /// waits (`retried`: it came, or it didn't, and this is the second look).
    pub(super) fn mail_read(&mut self, args: &Value, retried: bool) -> Step {
        let key = match self.mail_read_key(args) {
            Ok(k) => k,
            Err(e) => return Step::Done(Err(e)),
        };
        let Some(raw) = self.body_raw(&key) else {
            if retried {
                return Step::Done(Err("the message didn't download".to_string()));
            }
            self.jobs.push(Job::FetchBody { key: key.clone() });
            return Step::Later(Waiting::Body { key, args: args.clone(), outcome: None });
        };
        let headers = Args::new(args, &["key", "headers"]).ok().and_then(|a| a.bool("headers").ok().flatten()).unwrap_or(false);
        Step::Done(self.read_raw(&key, &raw, headers))
    }

    /// The key a `mail_read` names, when it's of a message in a folder here.
    fn mail_read_key(&mut self, args: &Value) -> Result<MessageKey, String> {
        let a = Args::new(args, &["key", "headers"])?;
        a.bool("headers")?;
        let text = a.req_str("key", MAX_STRING)?;
        let key = MessageKey::from_id(text).ok_or_else(|| "`key` isn't a message key".to_string())?;
        let known =
            self.folder(&key.account, &key.folder).is_some() && self.folder_state(&key.account, &key.folder).messages.iter().any(|m| m.key == key);
        if known { Ok(key) } else { Err("no such message".to_string()) }
    }

    fn read_raw(&mut self, key: &MessageKey, raw: &[u8], headers: bool) -> Result<Value, String> {
        // No side effects: no keys learned from the message, no decryption asked for (a smart
        // card's PIN prompt), nothing remembered. An encrypted message is opened in MailCraft.
        let (shown, locked) = self.unwrap_quiet(key, raw);
        if locked {
            let note = "this message is encrypted: open it in MailCraft first (it asks for your key), then read it again";
            return Ok(json!({"key": key.to_id(), "encrypted": true, "text": null, "note": note}));
        }
        let parsed = self.parse_for_reading(key, &shown).ok_or_else(|| "that message can't be read".to_string())?;
        let warning = self.message_warning(key, &parsed.from, parsed.bulk);
        let external = self.is_external(&parsed.from.email);
        let cut = parsed.text.char_indices().nth(MAX_TEXT).map(|(i, _)| i);
        let text = cut.and_then(|i| parsed.text.get(..i)).unwrap_or(&parsed.text);
        let mut out = json!({
            "key": key.to_id(),
            "from": parsed.from.to_string(),
            "to": mailcraft_model::addr::format_list(&parsed.to),
            "cc": mailcraft_model::addr::format_list(&parsed.cc),
            "date": parsed.date,
            "subject": parsed.subject,
            "text": text,
            "truncated": cut.is_some(),
            "attachments": parsed.attachments.iter().filter(|x| !x.inline).map(|x| json!({"index": x.index, "name": x.name, "mime": x.mime, "size": x.size})).collect::<Vec<_>>(),
            "external": external,
            "warning": warning,
        });
        if headers {
            let end = mailcraft_mime::header_end(raw).map_or(raw.len(), |blank| blank.start);
            let head = raw.get(..end).unwrap_or_default();
            let mut block = String::from_utf8_lossy(head).into_owned();
            if block.len() > MAX_HEADERS {
                let mut at = MAX_HEADERS;
                while !block.is_char_boundary(at) {
                    at -= 1;
                }
                block.truncate(at);
            }
            let check = match parsed.sender_check {
                mailcraft_mime::SenderCheck::Confirmed => "confirmed",
                mailcraft_mime::SenderCheck::Failed => "failed",
                mailcraft_mime::SenderCheck::Unknown => "unknown",
            };
            out["headers"] = json!({
                "raw": block,
                "authenticationResults": mailcraft_mime::header_fields(head, "Authentication-Results").take(8).collect::<Vec<_>>(),
                "senderCheck": check,
            });
        }
        Ok(out)
    }

    pub(super) fn mail_commands(&mut self, args: &Value, perms: &BTreeSet<Permission>) -> Result<Value, String> {
        no_arguments(args)?;
        if !perms.contains(&Permission::Organize) {
            return Ok(json!({"commands": []}));
        }
        let registry = self.registry();
        let commands: Vec<Value> =
            RUNNABLE.iter().filter_map(|id| registry.get(id)).map(|c| json!({"id": c.id, "label": c.label, "params": c.params})).collect();
        Ok(json!({"commands": commands}))
    }

    pub(super) fn monitor_log(&mut self, args: &Value) -> Result<Value, String> {
        let a = Args::new(args, &["limit", "since", "errors_only"])?;
        let limit = usize::try_from(a.int("limit", 1, MAX_LOG as i64)?.unwrap_or(100)).unwrap_or(100);
        let since = a.int("since", i64::MIN, i64::MAX)?;
        let errors_only = a.bool("errors_only")?.unwrap_or(false);
        let source = self.mcp.log_source.ok_or_else(|| "this MailCraft keeps no log".to_string())?;
        let mut lines = source(&LogQuery { limit, since, errors_only });
        lines.truncate(limit.min(MAX_LOG));
        Ok(json!({"lines": lines}))
    }
}
