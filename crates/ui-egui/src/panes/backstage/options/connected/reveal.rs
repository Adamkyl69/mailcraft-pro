//! The token just made, shown once: the token itself, and how to hand it to Claude Desktop and
//! Claude Code, the snippets built from the command that really starts the bridge.

use mailcraft_engine::mcp::BridgeCommand;

use super::*;

/// What starts the bridge when the app didn't say (a build run by hand): the CLI on the path.
fn bridge_of(s: &Session) -> BridgeCommand {
    s.mcp.bridge.clone().unwrap_or_else(|| BridgeCommand { program: "mailcraft-cli".into(), args: vec!["mcp".into()] })
}

/// `claude_desktop_config.json`'s entry for MailCraft, with the token in its environment (written
/// by hand so the command comes first, as people read it).
pub(super) fn desktop_snippet(b: &BridgeCommand, token: &str) -> String {
    let text = |s: &str| serde_json::to_string(s).unwrap_or_default();
    let args: Vec<String> = b.args.iter().map(|a| format!("        {}", text(a))).collect();
    [
        "{".to_string(),
        "  \"mcpServers\": {".to_string(),
        "    \"mailcraft\": {".to_string(),
        format!("      \"command\": {},", text(&b.program)),
        "      \"args\": [".to_string(),
        args.join(",\n"),
        "      ],".to_string(),
        "      \"env\": {".to_string(),
        format!("        \"MAILCRAFT_MCP_TOKEN\": {}", text(token)),
        "      }".to_string(),
        "    }".to_string(),
        "  }".to_string(),
        "}".to_string(),
    ]
    .join("\n")
}

/// A word of a shell command, in double quotes when it holds anything a shell would split or read.
fn quoted(word: &str) -> String {
    let plain = !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || "_-./:\\@%+=,".contains(c));
    if plain { word.to_string() } else { format!("\"{}\"", word.replace('"', "\\\"")) }
}

/// The line that adds MailCraft to Claude Code.
pub(super) fn code_snippet(b: &BridgeCommand, token: &str) -> String {
    let args: Vec<String> = b.args.iter().map(|a| quoted(a)).collect();
    format!("claude mcp add mailcraft --env MAILCRAFT_MCP_TOKEN={token} -- {} {}", quoted(&b.program), args.join(" "))
}

/// A line saying what the code below is, the code in a box nothing can change, and Copy.
fn snippet(ui: &mut Ui, t: &Tokens, label: &str, code: &str, rows: usize) {
    ui.add_space(6.0);
    ui.label(RichText::new(label).color(t.text).size(12.5));
    let mut shown: &str = code;
    ui.add(egui::TextEdit::multiline(&mut shown).font(egui::TextStyle::Monospace).desired_rows(rows).desired_width(f32::INFINITY));
    if ui.button(tl!("Copy")).clicked() {
        ui.ctx().copy_text(code.to_string());
    }
}

/// Shown while the token is new, until Done: the app has it only from here.
pub(super) fn new_token(s: &Session, t: &Tokens, ui: &mut Ui, acts: &mut Vec<Action>) {
    let Some(r) = &s.mcp.revealed else { return };
    let bridge = bridge_of(s);
    egui::Frame::new().fill(t.accent_soft).corner_radius(8).inner_margin(12).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(tl!("Copy this token now: it won't be shown again.")).font(theme::bold(13.0)).color(t.text));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let mut shown: &str = &r.token;
            let room = (ui.available_width() - 90.0).max(120.0);
            ui.add(egui::TextEdit::singleline(&mut shown).font(egui::TextStyle::Monospace).desired_width(room));
            if ui.button(tl!("Copy")).clicked() {
                ui.ctx().copy_text(r.token.clone());
            }
        });
        let desktop = desktop_snippet(&bridge, &r.token);
        snippet(ui, t, tl!("For Claude Desktop, in claude_desktop_config.json:"), &desktop, desktop.lines().count().min(12));
        snippet(ui, t, tl!("For Claude Code:"), &code_snippet(&bridge, &r.token), 3);
        ui.add_space(4.0);
        // Decision 1: a program that can run shell commands as the user can go round the protections.
        let warning =
            tl!("An AI app that can run shell commands can get around these protections: keep its shell commands behind its permission prompts.");
        ui.add(egui::Label::new(RichText::new(warning).color(t.warn).size(12.0)).wrap());
        ui.add_space(8.0);
        if ui.button(tl!("Done")).clicked() {
            owner(acts, "mcp.dismissToken", json!({}));
        }
    });
    ui.add_space(8.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bridge(program: &str, args: &[&str]) -> BridgeCommand {
        BridgeCommand { program: program.into(), args: args.iter().map(|a| a.to_string()).collect() }
    }

    #[test]
    fn the_desktop_snippet_is_json_with_the_real_command() {
        let b = bridge(r"C:\Program Files\MailCraft\mailcraft-cli.exe", &["mcp", "--data", r"D:\my mail"]);
        let v: Value = serde_json::from_str(&desktop_snippet(&b, "mcp_x")).unwrap_or_default();
        let entry = &v["mcpServers"]["mailcraft"];
        assert_eq!(entry["command"], r"C:\Program Files\MailCraft\mailcraft-cli.exe");
        assert_eq!(entry["args"], json!(["mcp", "--data", r"D:\my mail"]));
        assert_eq!(entry["env"]["MAILCRAFT_MCP_TOKEN"], "mcp_x");
    }

    #[test]
    fn the_claude_code_line_quotes_what_a_shell_would_split() {
        let plain = code_snippet(&bridge("/usr/bin/mailcraft-cli", &["mcp"]), "mcp_x");
        assert_eq!(plain, "claude mcp add mailcraft --env MAILCRAFT_MCP_TOKEN=mcp_x -- /usr/bin/mailcraft-cli mcp");
        let spaced = code_snippet(&bridge("/opt/Mail Craft/mailcraft-cli", &["mcp", "--data", "/home/me/my mail"]), "mcp_x");
        assert!(spaced.ends_with("-- \"/opt/Mail Craft/mailcraft-cli\" mcp --data \"/home/me/my mail\""), "{spaced}");
        assert_eq!(quoted("a\"b"), "\"a\\\"b\"");
        assert_eq!(quoted(""), "\"\"");
    }
}
