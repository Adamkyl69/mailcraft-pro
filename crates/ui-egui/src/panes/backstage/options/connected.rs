//! Options' Connected apps card: the AI apps on this computer that may read, organize and send
//! mail through MailCraft. Their tokens, the approval code for sending, the rules for sending
//! without one, and the sends waiting for approval.
//!
//! Every button asks for a command through [`Action::RunAsOwner`]: these commands run only from
//! a click here, so a script or an AI app can't hand itself a token or switch approval off.

use mailcraft_engine::mcp::{MAX_TOKENS, Permission};
use mailcraft_engine::model::dates;
use serde_json::Value;

use super::*;

mod approval;
mod reveal;

/// The card. Not in the browser: it has no socket for an AI app to reach.
pub(super) fn connected(s: &Session, st: &UiState, ui: &mut Ui, acts: &mut Vec<Action>) {
    if s.jmap_only {
        return;
    }
    let t = st.tokens;
    let about = tl!(
        "AI apps on this computer, such as Claude Desktop and Claude Code, can read and organize your mail in MailCraft while it's running. Each app gets a token of its own. Nothing is reachable from another computer."
    );
    card(ui, &t, tl!("Connected apps"), Some(about), |ui| {
        // The settings couldn't be read: they're left as they are, and so is the card.
        let unread = s.mcp_unusable().is_some();
        if unread {
            ui.label(
                RichText::new(tl!("The settings for connected apps couldn't be read, so nothing here can be changed.")).color(t.danger).size(12.5),
            );
            ui.add_space(4.0);
        }
        ui.add_enabled_ui(!unread, |ui| {
            master(s, &t, ui, acts);
            tokens(s, &t, ui, acts);
            reveal::new_token(s, &t, ui, acts);
            approval::approval(s, &t, ui, acts);
            rules(s, &t, ui, acts);
            pending(s, &t, ui, acts);
        });
    });
}

/// A command only this card's own click runs.
fn owner(acts: &mut Vec<Action>, id: &'static str, params: Value) {
    acts.push(Action::RunAsOwner(id, params));
}

/// A text kept while it's typed (the card is drawn again every frame).
fn kept_text(ui: &Ui, key: &'static str) -> (egui::Id, String) {
    let id = egui::Id::unique(key);
    (id, ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_default())
}

/// The switch: connected apps allowed at all. Off, every call is refused as a revoked token is.
fn master(s: &Session, t: &Tokens, ui: &mut Ui, acts: &mut Vec<Action>) {
    let on = s.mcp.settings.enabled;
    // Turning it on needs a token to use it with.
    let can = on || !s.mcp.settings.tokens.is_empty();
    let label = tl!("Allow connected apps");
    let about = (!can).then(|| tl!("Make a token first"));
    setting(ui, t, true, label, about, |ui| {
        ui.add_enabled_ui(can, |ui| {
            let mut v = on;
            if widgets::switch(ui, t, &mut v, label).changed() {
                owner(acts, "mcp.setEnabled", json!({"on": v}));
            }
        });
    });
}

fn permission_name(p: Permission) -> &'static str {
    match p {
        Permission::Read => tl!("Read"),
        Permission::Organize => tl!("Organize"),
        Permission::Send => tl!("Send"),
    }
}

/// A moment, in the day and time the rest of the interface writes.
fn moment(s: &Session, at: i64) -> String {
    crate::i18n::date(&dates::full_time(at, &s.clock))
}

/// Each token: its name, what it may do, when it was made and last used, and Revoke. Then the
/// form that makes another.
fn tokens(s: &Session, t: &Tokens, ui: &mut Ui, acts: &mut Vec<Action>) {
    let list = &s.mcp.settings.tokens;
    if list.is_empty() {
        ui.add_space(4.0);
        ui.label(RichText::new(tl!("No tokens yet: make one for each app you connect.")).color(t.faint).small());
        ui.add_space(8.0);
    }
    for tok in list {
        let permissions = tok.permissions.iter().map(|p| permission_name(*p)).collect::<Vec<_>>().join(", ");
        let made = tf!(
            "Made {date}",
            date = crate::i18n::date(&dates::day_text(tok.created.saturating_add(i64::from(s.clock.offset)).div_euclid(86_400), &s.clock))
        );
        let used = match tok.last_used {
            Some(at) => tf!("Last used {date}", date = moment(s, at)),
            None => tl!("Never used").to_string(),
        };
        setting(ui, t, false, &tok.name, Some(&format!("{permissions} · {made} · {used}")), |ui| {
            if ui.button(tl!("Revoke")).clicked() {
                owner(acts, "mcp.revokeToken", json!({"id": tok.id}));
            }
        });
    }
    // The form: a name, what the app may do (reading is always allowed), and Make token.
    let (name_id, mut name) = kept_text(ui, "mcp-token-name");
    let organize_id = egui::Id::unique("mcp-token-organize");
    let send_id = egui::Id::unique("mcp-token-send");
    let mut organize: bool = ui.data_mut(|d| d.get_temp(organize_id)).unwrap_or(false);
    let mut send: bool = ui.data_mut(|d| d.get_temp(send_id)).unwrap_or(false);
    ui.add_space(6.0);
    let mut make = false;
    ui.horizontal_wrapped(|ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut name).hint_text("Claude Desktop").char_limit(64).desired_width(200.0));
        let full = list.len() >= MAX_TOKENS;
        make |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !name.trim().is_empty() && !full;
        ui.add_enabled(false, egui::Checkbox::new(&mut true, tl!("Read")));
        ui.checkbox(&mut organize, tl!("Organize"));
        ui.checkbox(&mut send, tl!("Send"));
        make |= ui.add_enabled(!name.trim().is_empty() && !full, egui::Button::new(tl!("Make token"))).clicked();
    });
    if make {
        let mut permissions = vec!["read"];
        if organize {
            permissions.push("organize");
        }
        if send {
            permissions.push("send");
        }
        owner(acts, "mcp.createToken", json!({"name": name.trim(), "permissions": permissions}));
        name.clear();
    }
    ui.data_mut(|d| {
        d.insert_temp(name_id, name);
        d.insert_temp(organize_id, organize);
        d.insert_temp(send_id, send);
    });
    ui.add_space(6.0);
}

/// The rules for sending without a code, and how many such sends a day.
fn rules(s: &Session, t: &Tokens, ui: &mut Ui, acts: &mut Vec<Action>) {
    let rules = &s.mcp.settings.rules;
    let label = tl!("Send replies without a code");
    let about = tl!(
        "A reply in a conversation you've written in, from the account it came to, to people you've written to or saved as contacts, with no attachments, and nothing MailCraft would warn about."
    );
    setting(ui, t, false, label, Some(about), |ui| {
        let mut v = rules.no_code_replies;
        if widgets::switch(ui, t, &mut v, label).changed() {
            owner(acts, "mcp.setRules", json!({"noCodeReplies": v}));
        }
    });
    setting(ui, t, false, tl!("Most sends a day without a code"), Some(tl!("What you approve with a code isn't counted")), |ui| {
        let mut n = rules.daily_limit;
        // Typed, not dragged: each change is kept, so a drag would save a file at every step.
        let r = ui.add(egui::DragValue::new(&mut n).range(0..=200).speed(0.0).update_while_editing(false));
        if r.changed() && n != rules.daily_limit {
            owner(acts, "mcp.setRules", json!({"dailyLimit": n}));
        }
    });
}

/// The sends an AI app asked for that wait for a code: sent or discarded here instead.
fn pending(s: &Session, t: &Tokens, ui: &mut Ui, acts: &mut Vec<Action>) {
    use mailcraft_engine::SendCheck;
    if s.mcp.pending.is_empty() {
        return;
    }
    ui.add_space(8.0);
    ui.label(RichText::new(tl!("Waiting for approval")).font(theme::bold(13.5)).color(t.text));
    for (i, p) in s.mcp.pending.iter().enumerate() {
        let list =
            |key: &str| -> Vec<&str> { p.summary.get(key).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect() };
        let recipients = list("to").into_iter().chain(list("cc")).collect::<Vec<_>>().join(", ");
        let subject = p.summary.get("subject").and_then(Value::as_str).map(str::trim).filter(|x| !x.is_empty());
        let subject = subject.map_or_else(|| tl!("(no subject)").to_string(), str::to_string);
        // What was found about the recipients, as the check before sending words it.
        let found: Vec<SendCheck> = p.summary.get("warnings").cloned().and_then(|w| serde_json::from_value(w).ok()).unwrap_or_default();
        let mut lines: Vec<String> = found
            .iter()
            .map(|f| match f {
                SendCheck::Lookalike { address, like } => {
                    tf!("{address} looks like an address at {like}, but isn't one. Is it the right one?", address = address, like = like)
                }
                SendCheck::Exposed { count } => {
                    tf!("{n} people who may not know each other will all see each other's addresses.", n = count)
                }
            })
            .collect();
        lines.push(tf!("Expires {when}", when = moment(s, p.expires_ms.div_euclid(1000))));
        let label = tf!("To {recipients}: {subject}", recipients = recipients, subject = subject);
        setting(ui, t, i == 0, &label, Some(&lines.join("\n")), |ui| {
            if ui.button(tl!("Discard")).clicked() {
                owner(acts, "mcp.discardPending", json!({"id": p.id}));
            }
            if ui.button(tl!("Send")).clicked() {
                owner(acts, "mcp.sendPending", json!({"id": p.id}));
            }
        });
    }
}
