//! The approval code: set up with a QR code and the secret as text (any authenticator app reads
//! either), confirmed with a first code, then reset for a lost phone or unlocked after wrong codes.

use mailcraft_engine::mcp::approval::ApprovalState;
use qrcodegen::{QrCode, QrCodeEcc};

use super::*;

/// The quiet zone round the code, modules: a scanner needs four.
const QUIET: i32 = 4;
/// Across, at most (points); a module is never smaller than 4 so a phone's camera can read it.
const QR_SIZE: f32 = 220.0;

pub(super) fn approval(s: &Session, t: &Tokens, ui: &mut Ui, acts: &mut Vec<Action>) {
    let label = tl!("Approval for sending");
    let rec = &s.mcp.settings.approval;
    if let Some(setup) = &s.mcp.setup {
        setting_up(t, ui, label, &setup.uri, &setup.secret_b32, acts);
        return;
    }
    let active = rec.state == ApprovalState::Active;
    let missing = active && s.mcp_wants_secret();
    let about = if !active {
        tl!("Sending outside the rules below needs a 6-digit code from an authenticator app on your phone.").to_string()
    } else if missing {
        tl!("The approval's secret isn't here: set it up again.").to_string()
    } else if rec.locked {
        format!("{}\n{}", tl!("Approval is on."), tl!("Sending is locked after 5 wrong codes."))
    } else {
        tl!("Approval is on.").to_string()
    };
    setting(ui, t, false, label, Some(&about), |ui| {
        // Against the end, last button first (the row is laid out from its end).
        if active && !missing && ui.button(tl!("Reset (a lost phone)")).clicked() {
            owner(acts, "mcp.resetApproval", json!({}));
        }
        if active && !missing && rec.locked && ui.button(tl!("Unlock")).clicked() {
            owner(acts, "mcp.unlockApproval", json!({}));
        }
        if !active && ui.button(tl!("Set up")).clicked() || missing && ui.button(tl!("Set up again")).clicked() {
            owner(acts, "mcp.setupApproval", json!({}));
        }
    });
    // A portable copy keeps no secret between starts (as it keeps no password).
    if s.portable {
        ui.label(
            RichText::new(tl!("This is a portable copy: approval is kept in memory only, so set it up again after each start."))
                .color(t.faint)
                .size(12.0),
        );
        ui.add_space(4.0);
    }
}

/// The set-up in progress: the QR code, the secret as text, and a box for the first code.
fn setting_up(t: &Tokens, ui: &mut Ui, label: &str, uri: &str, secret: &str, acts: &mut Vec<Action>) {
    ui.add_space(6.0);
    egui::Frame::new().fill(t.accent_soft).corner_radius(8).inner_margin(12).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(label).font(theme::bold(13.0)).color(t.text));
        ui.add_space(4.0);
        let how = tl!("Scan this code with an authenticator app, or type the secret into it:");
        ui.add(egui::Label::new(RichText::new(how).color(t.text).size(12.5)).wrap());
        ui.add_space(6.0);
        qr(ui, uri);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(RichText::new(grouped(secret)).monospace().color(t.text)).selectable(true));
            if ui.button(tl!("Copy")).clicked() {
                ui.ctx().copy_text(secret.to_string());
            }
        });
        ui.add_space(8.0);
        let (id, mut code) = kept_text(ui, "mcp-approval-code");
        let mut confirm = false;
        ui.horizontal_wrapped(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut code).hint_text(tl!("6-digit code")).char_limit(8).desired_width(120.0));
            confirm |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !code.trim().is_empty();
            confirm |= ui.add_enabled(!code.trim().is_empty(), egui::Button::new(tl!("Confirm"))).clicked();
            // Nothing is on yet, so putting it back to Off is all a cancel is.
            if ui.button(tl!("Cancel")).clicked() {
                owner(acts, "mcp.resetApproval", json!({}));
            }
        });
        if confirm {
            owner(acts, "mcp.confirmApproval", json!({"code": code.trim()}));
            code.clear();
        }
        ui.data_mut(|d| d.insert_temp(id, code));
    });
    ui.add_space(8.0);
}

/// The secret in groups of four, as it's easier to type in.
fn grouped(secret: &str) -> String {
    secret.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ")
}

/// The dark modules of a QR code, row by row: `(x, y, run)` for each run of them in a row (a run
/// is one rectangle, so no seams show between neighbours).
fn runs(code: &QrCode) -> Vec<(i32, i32, i32)> {
    let n = code.size();
    let mut out = Vec::new();
    for y in 0..n {
        let mut x = 0;
        while x < n {
            if !code.get_module(x, y) {
                x += 1;
                continue;
            }
            let start = x;
            while x < n && code.get_module(x, y) {
                x += 1;
            }
            out.push((start, y, x - start));
        }
    }
    out
}

/// The QR code, dark modules on white in both themes (a phone reads dark on light), with its
/// quiet zone. Not drawn when the text doesn't fit one (it always does: the address is short).
fn qr(ui: &mut Ui, text: &str) {
    let Ok(code) = QrCode::encode_text(text, QrCodeEcc::Medium) else { return };
    let total = code.size() + 2 * QUIET;
    let module = (QR_SIZE / total as f32).floor().clamp(4.0, 8.0);
    let side = module * total as f32;
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
    let label = tl!("QR code for the authenticator app");
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, label));
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::WHITE);
    for (x, y, run) in runs(&code) {
        let min = rect.min + vec2((x + QUIET) as f32 * module, (y + QUIET) as f32 * module);
        painter.rect_filled(egui::Rect::from_min_size(min, vec2(run as f32 * module, module)), 0.0, egui::Color32::BLACK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_secret_reads_in_groups_of_four() {
        assert_eq!(grouped("ABCDEFGHIJ"), "ABCD EFGH IJ");
        assert_eq!(grouped(""), "");
    }

    #[test]
    fn the_runs_are_exactly_the_dark_modules() {
        let uri = "otpauth://totp/MailCraft?secret=JBSWY3DPEHPK3PXP&issuer=MailCraft";
        let Ok(code) = QrCode::encode_text(uri, QrCodeEcc::Medium) else { panic!("the address fits a code") };
        let dark: usize = (0..code.size()).map(|y| (0..code.size()).filter(|x| code.get_module(*x, y)).count()).sum();
        assert_eq!(runs(&code).iter().map(|r| r.2 as usize).sum::<usize>(), dark);
    }

    /// Edge case 42: the longest address (a 64-byte secret as base32) fits version 10 or less.
    #[test]
    fn the_longest_address_fits_a_small_code() {
        let secret = "A".repeat(103);
        let uri = mailcraft_engine::mcp::approval::otpauth_uri(&secret);
        let Ok(code) = QrCode::encode_text(&uri, QrCodeEcc::Medium) else { panic!("the address fits a code") };
        assert!(code.version().value() <= 10, "version {}", code.version().value());
    }
}
