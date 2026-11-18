//! `mail_send`: a message built off screen, checked against the rules, sent at once or held for
//! the owner's code, and the code that releases it. Nothing here touches the message the user is
//! writing: everything is made and sent inside [`Session::aside`].

use super::*;
use crate::mcp::approval::{self, Verdict};
use crate::mcp::rules::{self, RuleFacts, RuleMiss};
use crate::mcp::{MAX_BODY, MAX_PENDING, MAX_RECIPIENTS, MAX_STRING, PENDING_MS};

/// Why a send didn't go: it was asked wrongly, or it isn't allowed.
enum Fail {
    Tool(String),
    Refused(String),
}

impl From<String> for Fail {
    fn from(e: String) -> Fail {
        Fail::Tool(e)
    }
}

fn refused(why: &str) -> Fail {
    Fail::Refused(why.to_string())
}

fn error_text(e: CmdError) -> String {
    match e {
        CmdError::Params(m) | CmdError::Disabled(m) | CmdError::Failed(m) => m,
        CmdError::Unknown(c) => format!("`{c}` isn't a command"),
    }
}

/// What the call asked to be built.
struct Wanted {
    /// To and Cc as given (`None`: not given; a reply then keeps its own).
    to: Option<String>,
    cc: Option<String>,
    subject: Option<String>,
    body: String,
    attachments: Vec<String>,
    from: Option<String>,
    reply: Option<MessageKey>,
}

const LOCKED: &str = "sending is locked: unlock it in MailCraft";

impl Session {
    pub(super) fn mail_send(&mut self, args: &Value, token_id: &str, now_ms: i64) -> Step {
        match self.send_call(args, token_id, now_ms) {
            Ok(step) => step,
            Err(Fail::Tool(e)) => Step::Done(Err(e)),
            Err(Fail::Refused(e)) => Step::Refused(e),
        }
    }

    fn send_call(&mut self, args: &Value, token_id: &str, now_ms: i64) -> Result<Step, Fail> {
        let a = Args::new(args, &["to", "cc", "subject", "body", "attachments", "from", "in_reply_to", "pending_id", "approval_code"])?;
        if a.has("pending_id") || a.has("approval_code") {
            // Approving a held send is all it says: a code can't change what was held.
            if a.names().any(|n| n != "pending_id" && n != "approval_code") || !(a.has("pending_id") && a.has("approval_code")) {
                return Err("approving takes `pending_id` and `approval_code` and nothing else".to_string().into());
            }
            return self.approve_pending(a.req_str("pending_id", 64)?, a.req_str("approval_code", 64)?, token_id, now_ms);
        }
        let people = |name: &str| -> Result<Vec<mailcraft_model::EmailAddress>, String> {
            let mut all = Vec::new();
            for item in a.string_or_list(name, 100)? {
                all.extend(mailcraft_model::addr::parse_list(item).map_err(|bad| format!("`{name}`: not an email address: {bad}"))?);
            }
            Ok(all)
        };
        let (to, cc) = (people("to")?, people("cc")?);
        let reply = match a.str("in_reply_to", MAX_STRING)? {
            Some(text) => Some(MessageKey::from_id(text).ok_or_else(|| "`in_reply_to` isn't a message key".to_string())?),
            None => None,
        };
        if to.is_empty() && reply.is_none() {
            return Err("`to` is required for a new message".to_string().into());
        }
        if to.len() + cc.len() > MAX_RECIPIENTS {
            return Err(refused("more than 10 recipients"));
        }
        if self.mcp.settings.approval.locked {
            return Err(refused(LOCKED));
        }
        let given = |name: &str, list: &[mailcraft_model::EmailAddress]| a.has(name).then(|| mailcraft_model::addr::format_list(list));
        let wanted = Wanted {
            to: given("to", &to),
            cc: given("cc", &cc),
            subject: a.str("subject", MAX_STRING)?.map(str::to_string),
            body: a.str("body", MAX_BODY)?.ok_or_else(|| "`body` is required".to_string())?.to_string(),
            attachments: a.strings("attachments", 0, 10)?.unwrap_or_default().into_iter().map(str::to_string).collect(),
            from: a.str("from", MAX_STRING)?.map(str::to_string),
            reply,
        };
        let (c, from_receiving) = self.build_for_mcp(&wanted)?;
        let d = self.draft_of(&c, false).map_err(error_text)?;
        if d.recipients().is_empty() {
            return Err("add at least one recipient".to_string().into());
        }
        if d.to.len() + d.cc.len() > MAX_RECIPIENTS {
            return Err(refused("more than 10 recipients"));
        }
        let recipients: Vec<String> = d.to.iter().chain(&d.cc).map(|r| r.email.trim().to_lowercase()).collect();
        let checks = self.send_checks(&c, &d);
        let today = mailcraft_model::dates::today(&self.clock);
        let settings = &self.mcp.settings;
        let facts = RuleFacts {
            rules_on: settings.rules.no_code_replies,
            is_reply: wanted.reply.is_some(),
            owner_wrote_in_thread: false,
            from_receiving_account: from_receiving,
            unknown_recipients: Vec::new(),
            outside_conversation: Vec::new(),
            attachments: c.attachments.len(),
            checks: checks.len(),
            sent_today: if settings.sent_today.0 == today { settings.sent_today.1 } else { 0 },
            daily_limit: settings.rules.daily_limit,
        };
        let facts = RuleFacts {
            owner_wrote_in_thread: wanted.reply.as_ref().is_some_and(|k| self.owner_wrote_in_thread(k)),
            unknown_recipients: self.written_to_before(&recipients),
            outside_conversation: wanted.reply.as_ref().map(|k| self.outside_message(k, &recipients)).unwrap_or_default(),
            ..facts
        };
        let misses = rules::misses(&facts);
        if misses.is_empty() {
            return self.hand_over(c, false, true, token_id, now_ms);
        }
        let warnings = serde_json::to_value(&checks).unwrap_or(Value::Null);
        self.hold_send(c, misses, warnings, token_id, now_ms)
    }

    /// The message made off screen: a reply to a message (not marked read, the user's pane left
    /// alone) or a new one, with the text, addresses and files asked for. `true` with it when a
    /// reply comes from the account the original came to, from the address the reply chose.
    fn build_for_mcp(&mut self, w: &Wanted) -> Result<(Compose, bool), Fail> {
        if let Some(key) = &w.reply {
            let known = self.folder(&key.account, &key.folder).is_some()
                && self.folder_state(&key.account, &key.folder).messages.iter().any(|m| m.key == *key);
            if !known {
                return Err("no such message".to_string().into());
            }
            if self.body_raw(key).is_none() {
                return Err("read it first with mail_read (it isn't downloaded yet)".to_string().into());
            }
        }
        let built = self.aside(false, |s| -> Result<(Compose, bool), String> {
            let mut chosen = None;
            let mut fields = serde_json::Map::new();
            let own = match &w.reply {
                Some(key) => {
                    s.open_view(View::Folder { account: key.account.clone(), folder: key.folder.clone() });
                    s.show_message(key, false);
                    s.finish_pending_body();
                    s.reply_compose(ReplyMode::Reply).map_err(error_text)?;
                    chosen = s.compose.as_ref().map(|c| c.from.clone());
                    let own = s.compose.as_ref().map(|c| c.body.clone()).unwrap_or_default();
                    format!("\n\n{own}")
                }
                None => {
                    s.run("mail.new", &json!({})).map_err(error_text)?;
                    s.compose.as_ref().map(|c| c.body.clone()).unwrap_or_default()
                }
            };
            for (name, value) in [("to", &w.to), ("cc", &w.cc)] {
                if let Some(v) = value {
                    fields.insert(name.to_string(), json!(v));
                }
            }
            match (&w.subject, &w.reply) {
                (Some(subject), _) => drop(fields.insert("subject".to_string(), json!(subject))),
                (None, None) => drop(fields.insert("subject".to_string(), json!(""))),
                (None, Some(_)) => {}
            }
            fields.insert("body".to_string(), json!(format!("{}{own}", w.body)));
            s.run("compose.update", &Value::Object(fields)).map_err(error_text)?;
            if let Some(from) = &w.from {
                let params = if from.contains('@') {
                    let account = s.accounts.iter().find(|a| a.config.has_address(from)).map(|a| a.config.id.clone());
                    let account = account.ok_or_else(|| format!("{from} isn't an address of any account"))?;
                    json!({"account": account, "from": from})
                } else {
                    json!({"account": from})
                };
                s.run("compose.update", &params).map_err(error_text)?;
            }
            for path in &w.attachments {
                s.run("compose.attach", &json!({"path": path})).map_err(error_text)?;
            }
            let c = s.compose.take().ok_or_else(|| "no message was made".to_string())?;
            let from_receiving = w.reply.as_ref().is_some_and(|k| c.account == k.account && chosen.as_deref() == Some(c.from.as_str()));
            Ok((c, from_receiving))
        })?;
        Ok(built?)
    }

    /// Hold `c` for the owner's code: the answer says what it is and why a code is needed.
    fn hold_send(&mut self, c: Compose, reasons: Vec<RuleMiss>, warnings: Value, token_id: &str, now_ms: i64) -> Result<Step, Fail> {
        if self.mcp.settings.approval.state != ApprovalState::Active {
            return Err(refused("this message needs your approval, and approval isn't set up (File › Options › Connected apps)"));
        }
        if self.mcp.pending.len() >= MAX_PENDING {
            return Err(refused("too many sends are waiting for approval"));
        }
        let d = self.draft_of(&c, false).map_err(error_text)?;
        let size = self.compose_bytes(&c, &d).map_err(error_text)?.0.len();
        let id = match self.mcp.random.and_then(|random| random(8)).filter(|b| b.len() == 8) {
            Some(bytes) => bytes.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            None => format!("{:016x}", self.fresh_id()),
        };
        let from = self.account(&c.account).map(|a| a.config.sender(&c.from).email).unwrap_or_default();
        let addresses = |list: &[mailcraft_model::EmailAddress]| list.iter().map(|a| a.email.trim().to_string()).collect::<Vec<_>>();
        let summary = json!({
            "from": from,
            "to": addresses(&d.to),
            "cc": addresses(&d.cc),
            "subject": d.subject,
            "size": size,
            "attachments": c.attachments.iter().map(|a| json!({"name": a.name, "size": a.data.len()})).collect::<Vec<_>>(),
            "warnings": warnings,
            "reasons": reasons,
        });
        let expires_ms = now_ms.saturating_add(PENDING_MS);
        let answer = json!({"outcome": "needsCode", "pendingId": id, "expiresAt": expires_ms.div_euclid(1000), "summary": summary});
        self.mcp.pending.push(PendingSend { id, token_id: token_id.to_string(), compose: c, created_ms: now_ms, expires_ms, summary });
        self.notice("An AI app asks to send a message: approve it with a code, or in File › Options", false);
        Ok(Step::Done(Ok(answer)))
    }

    /// Send the frozen message `c` now. `checked`: the owner (or a code) has seen it, so what
    /// `send_checks` finds doesn't hold it. `automatic`: no code was needed, so it counts for the day.
    fn hand_over(&mut self, c: Compose, checked: bool, automatic: bool, token_id: &str, now_ms: i64) -> Result<Step, Fail> {
        let id = c.id;
        let answer = self.send_frozen(&c, checked).map_err(Fail::Tool)?;
        if let Some(found) = answer.get("check") {
            // The rules found it clean, and the send's own look didn't: held for a code, not sent.
            return self.hold_send(c, vec![RuleMiss::SendChecks], found.clone(), token_id, now_ms);
        }
        if automatic {
            let today = mailcraft_model::dates::today(&self.clock);
            let count = if self.mcp.settings.sent_today.0 == today { self.mcp.settings.sent_today.1 } else { 0 };
            self.mcp.settings.sent_today = (today, count.saturating_add(1));
            if let Err(e) = self.save_mcp() {
                log::warn!(target: "mailcraft::mcp", "the settings couldn't be saved: {e}");
            }
        }
        let has = |k: &str| answer.get(k).is_some();
        if has("sent") {
            Ok(Step::Done(Ok(json!({"outcome": "sent", "id": id}))))
        } else if has("queued") {
            Ok(Step::Done(Ok(json!({"outcome": "queued", "id": id, "error": "kept in the Outbox: it goes out later"}))))
        } else if has("sending") || has("protecting") {
            self.owe_reply_flag(&c);
            Ok(Step::Later(Waiting::Send { compose_id: id, outcome: None }))
        } else {
            Err(Fail::Tool("the message couldn't be handed over".to_string()))
        }
    }

    /// A message going out over the network is gone from the screen by the time the server
    /// answers, so the flag its original earns (Replied or Forwarded) is owed through the same
    /// list the Outbox's replies use.
    fn owe_reply_flag(&mut self, c: &Compose) {
        if let (Some(src), Some(mode)) = (&c.source, c.mode)
            && self.pending_marks.len() < 1000
        {
            let flag = if mode == ReplyMode::Forward { Flag::Forwarded } else { Flag::Answered };
            self.pending_marks.insert(c.id, (src.clone(), flag));
            self.save_pending_marks();
        }
    }

    /// `c` handed to the send, off screen: what the send answers (`{"sent": id}`, `{"sending": id}`,
    /// `{"check": ...}`...). The user's own message is untouched, and none is kept to open again
    /// if the send fails.
    fn send_frozen(&mut self, c: &Compose, checked: bool) -> Result<Value, String> {
        let sent = self.aside(false, |s| {
            s.compose = Some(c.clone());
            s.send_compose(true, checked)
        })?;
        sent.map_err(error_text)
    }

    /// A code for a held send: the right one releases it, as it was frozen.
    fn approve_pending(&mut self, pending_id: &str, code: &str, token_id: &str, now_ms: i64) -> Result<Step, Fail> {
        let at = self.mcp.pending.iter().position(|p| p.id == pending_id && p.token_id == token_id);
        let Some(at) = at else { return Err("no such pending send: it may have expired".to_string().into()) };
        if self.mcp.settings.approval.locked {
            return Err(refused(LOCKED));
        }
        let Some(secret) = self.mcp.secret.clone() else {
            return Err(refused("approval isn't ready: MailCraft is still reading it from the keychain, or it must be set up again"));
        };
        let code = approval::parse_code(code).ok_or_else(|| "the code is six digits".to_string())?;
        let mut record = self.mcp.settings.approval.clone();
        let verdict = approval::check(&mut record, &secret, code, now_ms.div_euclid(1000));
        let newly_locked = record.locked && !self.mcp.settings.approval.locked;
        self.mcp.settings.approval = record;
        if let Err(e) = self.save_mcp() {
            log::warn!(target: "mailcraft::mcp", "the settings couldn't be saved: {e}");
        }
        match verdict {
            Verdict::Ok { .. } => {
                let held = self.mcp.pending.remove(at);
                self.hand_over(held.compose, true, false, token_id, now_ms)
            }
            Verdict::Wrong | Verdict::Replayed => {
                log::warn!(target: "mailcraft::mcp", "an approval code was wrong");
                if newly_locked {
                    log::warn!(target: "mailcraft::mcp", "sending locked after {} wrong codes", crate::mcp::LOCK_AFTER);
                    return Err("that code isn't right, and sending is now locked: unlock it in MailCraft".to_string().into());
                }
                Err("that code isn't right".to_string().into())
            }
            Verdict::Locked => Err(refused(LOCKED)),
            Verdict::NotSetUp => Err(refused("approval isn't set up")),
        }
    }

    /// The owner sends a held message from the desktop: as `compose.send` answers.
    pub(crate) fn mcp_send_pending(&mut self, id: &str) -> CmdResult {
        // One that ran out of time is gone, as it is for every call: it can't be sent late.
        self.drop_expired_pending(self.clock.now.saturating_mul(1000));
        let at =
            self.mcp.pending.iter().position(|p| p.id == id).ok_or_else(|| CmdError::Params("no such pending send: it may have expired".into()))?;
        let held = self.mcp.pending.remove(at);
        match self.send_frozen(&held.compose, true) {
            Ok(answer) => {
                if answer.get("sending").is_some() || answer.get("protecting").is_some() {
                    self.owe_reply_flag(&held.compose);
                }
                Ok(answer)
            }
            Err(e) => {
                self.mcp.pending.insert(at.min(self.mcp.pending.len()), held);
                Err(CmdError::Failed(e))
            }
        }
    }

    /// Everything held is let go (approval was reset).
    pub(crate) fn mcp_discard_pending(&mut self, id: Option<&str>) -> bool {
        let before = self.mcp.pending.len();
        self.mcp.pending.retain(|p| id.is_some_and(|i| p.id != i));
        self.mcp.pending.len() < before
    }
}
