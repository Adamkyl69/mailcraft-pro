//! File › Options › Connected apps: the card in each state, light and dark (the tour runs in
//! both, and in the pseudo-language the translation tests use), and what its buttons do.

use mailcraft_engine::mcp::BridgeCommand;
use mailcraft_engine::mcp::approval::{code_at, step, unbase32};
use mailcraft_engine::{PendingSend, Session};
use serde_json::{Value, json};

use super::*;
use crate::Action;
use crate::backstage::Page;

/// A source of "random" bytes that differ from call to call (tokens made here must differ).
fn random(n: usize) -> Option<Vec<u8>> {
    static CALLS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);
    let k = CALLS.fetch_add(7, std::sync::atomic::Ordering::Relaxed);
    Some((0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(k)).collect())
}

/// What the app gives the session: a random source and the command that starts the bridge.
fn wired(a: &mut MailApp) {
    a.session.mcp.random = Some(random);
    a.session.mcp.bridge = Some(BridgeCommand { program: "/opt/mailcraft/mailcraft-cli".into(), args: vec!["mcp".into()] });
}

/// A click in the card.
fn owner(a: &mut MailApp, id: &'static str, params: Value) {
    a.act(Action::RunAsOwner(id, params));
}

fn options(ctx: &egui::Context, a: &mut MailApp) {
    a.act(Action::Backstage(Page::Options));
    frame(ctx, a, vec![]);
}

/// The code the authenticator app would show now for the secret being set up.
fn code_now(a: &MailApp) -> String {
    let secret = a.session.mcp.setup.as_ref().map(|s| s.secret_b32.clone()).unwrap_or_default();
    let bytes = unbase32(&secret).unwrap_or_default();
    format!("{:06}", code_at(&bytes, step(NOW)).unwrap_or_default())
}

fn held(id: &str, to: &[&str], subject: &str, warnings: Value) -> PendingSend {
    PendingSend {
        id: id.into(),
        token_id: "000000000000".into(),
        compose: Default::default(),
        created_ms: NOW * 1000,
        expires_ms: NOW * 1000 + 600_000,
        summary: json!({"from": "riley@studio.example", "to": to, "cc": [], "subject": subject, "size": 1200, "attachments": [], "warnings": warnings, "reasons": []}),
    }
}

/// Every state of the card, each drawn: nothing yet, a token just made (shown once), approval
/// being set up (the QR code), on, locked, its secret gone, a portable copy, sends waiting, a full
/// list of long names, and settings that couldn't be read.
pub(crate) fn tour(a: &mut MailApp, ctx: &egui::Context) {
    wired(a);
    options(ctx, a);
    owner(a, "mcp.createToken", json!({"name": "Claude Desktop", "permissions": ["read", "organize"]}));
    options(ctx, a);
    owner(a, "mcp.dismissToken", json!({}));
    // Where the app packages no bridge (macOS): the token, and a line saying why no snippet.
    let bridge = a.session.mcp.bridge.take();
    owner(a, "mcp.createToken", json!({"name": "Claude Desktop", "permissions": ["read"]}));
    options(ctx, a);
    owner(a, "mcp.dismissToken", json!({}));
    a.session.mcp.bridge = bridge;
    owner(a, "mcp.setEnabled", json!({"on": true}));
    owner(a, "mcp.setupApproval", json!({}));
    options(ctx, a);
    let code = code_now(a);
    owner(a, "mcp.confirmApproval", json!({"code": code}));
    options(ctx, a);
    a.session.mcp.settings.approval.locked = true;
    options(ctx, a);
    owner(a, "mcp.unlockApproval", json!({}));
    a.session.apply(MailEvent::AppSecretRead { what: "mcp-approval".into(), secret: None });
    options(ctx, a);
    a.session.portable = true;
    options(ctx, a);
    a.session.portable = false;
    a.session.mcp.pending = vec![
        held("a1", &["ana@contoso.example"], "Re: the schedule", json!([])),
        held(
            "a2",
            &["bob@printsh0p.example", "carol@studio.example"],
            "",
            json!([{"kind": "lookalike", "address": "bob@printsh0p.example", "like": "printshop.example"}, {"kind": "exposed", "count": 12}]),
        ),
    ];
    options(ctx, a);
    a.session.mcp.pending.clear();
    // The longest name, as many times as there can be.
    for i in 0..20 {
        let name: String = format!("{i:02} ").chars().chain("W".repeat(61).chars()).collect();
        owner(a, "mcp.createToken", json!({"name": name, "permissions": ["read", "organize", "send"]}));
    }
    owner(a, "mcp.dismissToken", json!({}));
    options(ctx, a);
    a.ui.backstage = None;
    // The tour goes on with connected apps as they were: none.
    a.session.mcp = Default::default();
}

/// Settings that couldn't be read are left as they are, and the card says so.
fn unread_settings(a: &mut MailApp, ctx: &egui::Context) {
    let Some(store) = a.session.store.clone() else { return };
    // A file that can't be read at all (the store sets a damaged one aside instead).
    let _ = std::fs::create_dir(store.root().join("mcp.json"));
    a.session = Session::open(Some(store), Clock::new(NOW, 0));
    assert!(a.session.mcp_unusable().is_some());
    options(ctx, a);
    owner(a, "mcp.setEnabled", json!({"on": true}));
    assert!(!a.session.mcp.settings.enabled, "nothing changes while the settings are unread");
    a.ui.backstage = None;
}

#[test]
fn the_card_is_drawn_in_every_state() {
    let (mut a, _) = app("connected-tour");
    let ctx = egui::Context::default();
    for dark in [Some(false), Some(true)] {
        a.ui.dark = dark;
        tour(&mut a, &ctx);
    }
    let (mut b, _) = app("connected-unread");
    unread_settings(&mut b, &ctx);
}

#[test]
fn a_click_runs_a_command_only_the_card_can_run() {
    let (mut a, _) = app("connected-owner");
    let ctx = egui::Context::default();
    wired(&mut a);
    options(&ctx, &mut a);
    // As a script or the command line would ask: refused.
    a.run("mcp.createToken", json!({"name": "Sneaky"}));
    assert!(a.session.mcp.settings.tokens.is_empty());
    // As the card's button does.
    owner(&mut a, "mcp.createToken", json!({"name": "Claude Desktop", "permissions": ["read"]}));
    assert_eq!(a.session.mcp.settings.tokens.len(), 1);
    assert!(a.session.mcp.revealed.is_some());
    assert!(!a.session.mcp.owner_ui, "the owner's click is over once the command has run");
    a.run("mcp.revokeToken", json!({"id": a.session.mcp.settings.tokens[0].id}));
    assert_eq!(a.session.mcp.settings.tokens.len(), 1, "a script can't revoke either");
}

/// With a bridge command the new token comes with its two snippets; without one (a macOS build),
/// a line saying the bridge isn't packaged, and no snippet that names a program that isn't there.
#[test]
fn the_card_says_when_there_is_no_bridge_to_name() {
    let (mut a, _) = app("connected-no-bridge");
    let ctx = egui::Context::default();
    wired(&mut a);
    let sentence = "MailCraft doesn't package the bridge for AI apps on this system yet, so there is no snippet to copy. The project's Known gaps page (docs/gaps.md) says what to do instead.";
    let shown = |a: &mut MailApp| {
        options(&ctx, a);
        // A window tall enough for the whole page: the card is its last one.
        let size = egui::vec2(1280.0, 9000.0);
        drawn_with(&ctx, a, size, vec![]).into_iter().map(|d| d.text).collect::<Vec<_>>().join("\n")
    };
    owner(&mut a, "mcp.createToken", json!({"name": "Claude Desktop"}));
    let with = shown(&mut a);
    assert!(with.contains("For Claude Code:") && !with.contains(sentence), "{with}");
    owner(&mut a, "mcp.dismissToken", json!({}));
    a.session.mcp.bridge = None;
    owner(&mut a, "mcp.createToken", json!({"name": "Claude Desktop"}));
    let without = shown(&mut a);
    assert!(without.contains(sentence), "{without}");
    assert!(!without.contains("For Claude Code:") && !without.contains("claude_desktop_config.json"), "no snippet without a program to name");
    assert!(without.contains("Copy this token now"), "the token is still shown");
}

#[test]
fn the_qr_code_is_dark_squares_on_white_in_both_themes() {
    let (mut a, _) = app("connected-qr");
    let ctx = egui::Context::default();
    wired(&mut a);
    owner(&mut a, "mcp.createToken", json!({"name": "Claude Desktop"}));
    owner(&mut a, "mcp.setupApproval", json!({}));
    for dark in [Some(false), Some(true)] {
        a.ui.dark = dark;
        // Two frames first: a page's first is only measured.
        options(&ctx, &mut a);
        frame(&ctx, &mut a, vec![]);
        let size = windows::layout::SIZE.with(std::cell::Cell::get);
        windows::layout::dress(&mut a);
        let input = RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| a.ui(ui));
        // No renderer here: drop the texture uploads deliberately.
        out.textures_delta.clear();
        let (mut white, mut black) = (0, 0);
        for clipped in &out.shapes {
            if let egui::Shape::Rect(r) = &clipped.shape {
                white += usize::from(r.fill == egui::Color32::WHITE && r.rect.width() >= 100.0 && (r.rect.width() - r.rect.height()).abs() < 0.5);
                black += usize::from(r.fill == egui::Color32::BLACK);
            }
        }
        assert_eq!(white, 1, "one light square behind the code, dark theme or not");
        assert!(black > 100, "the modules are drawn: {black}");
    }
}

/// Edge case 41: twenty apps, each with the longest name, drawn at the narrowest window MailCraft allows (760 wide): every
/// piece of text ends inside the window, and the card isn't a reason for a crash.
#[test]
fn edge_case_41_twenty_long_names_fit_the_card_at_narrow_widths() {
    let (mut a, _) = app("connected-narrow");
    let ctx = egui::Context::default();
    wired(&mut a);
    for i in 0..20 {
        let name: String = format!("{i:02} ").chars().chain("W".repeat(61).chars()).collect();
        owner(&mut a, "mcp.createToken", json!({"name": name, "permissions": ["read", "organize", "send"]}));
    }
    owner(&mut a, "mcp.dismissToken", json!({}));
    options(&ctx, &mut a);
    for width in [1280.0_f32, 900.0, crate::MIN_WINDOW_SIZE.x] {
        for dark in [Some(false), Some(true)] {
            a.ui.dark = dark;
            for _ in 0..2 {
                windows::layout::dress(&mut a);
                let input =
                    RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 800.0))), ..Default::default() };
                let mut out = ctx.run_ui(input, |ui| a.ui(ui));
                out.textures_delta.clear();
                for clipped in &out.shapes {
                    if let egui::Shape::Text(t) = &clipped.shape {
                        let right = t.pos.x + t.galley.rect.max.x;
                        assert!(right <= width + 1.0, "text reaches {right} in a window {width} wide: {:?}", t.galley.text());
                    }
                }
            }
        }
    }
}
