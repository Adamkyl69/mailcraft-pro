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
    let args: Vec<String> = b.args_with_token(token).iter().map(|a| format!("        {}", text(a))).collect();
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

/// A word of a shell command, quoted when it holds anything a shell would split or read: in
/// single quotes for a POSIX shell (where nothing inside them is special, a `'` becoming `'\''`),
/// in double quotes on Windows (cmd and PowerShell).
fn quoted(word: &str) -> String {
    quoted_for(word, cfg!(windows))
}

fn quoted_for(word: &str, windows: bool) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_-./:@%+=,".contains(c) || (windows && c == '\\');
    if !word.is_empty() && word.chars().all(safe) {
        word.to_string()
    } else if windows {
        format!("\"{}\"", windows_escaped(word))
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// `word` for inside double quotes as Windows programs read their command line (the rules of
/// `CommandLineToArgvW`): a `"` is `\"`, and the backslashes before a `"`, or before the closing
/// quote (a folder ending in `\`), are doubled so they don't escape it.
fn windows_escaped(word: &str) -> String {
    let mut out = String::new();
    let mut slashes = 0;
    for c in word.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out
}

/// The line that adds MailCraft to Claude Code.
pub(super) fn code_snippet(b: &BridgeCommand, token: &str) -> String {
    let args: Vec<String> = b.args_with_token(token).iter().map(|a| quoted(a)).collect();
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
        if !cfg!(windows) {
            let spaced = code_snippet(&bridge("/opt/Mail Craft/mailcraft-cli", &["mcp", "--data", "/home/me/my mail"]), "mcp_x");
            assert!(spaced.ends_with("-- '/opt/Mail Craft/mailcraft-cli' mcp --data '/home/me/my mail'"), "{spaced}");
        }
    }

    /// A Flatpak's bridge gets the token as an argument, since `flatpak run` doesn't pass the
    /// environment on; both snippets carry it there and in the environment.
    #[test]
    fn the_token_goes_where_the_command_asks_for_it() {
        let b = bridge(
            "flatpak",
            &["run", "--command=mailcraft-cli", mailcraft_engine::mcp::TOKEN_PLACEHOLDER_ARG, "io.github.zidan44.MailCraft", "mcp"],
        );
        let v: Value = serde_json::from_str(&desktop_snippet(&b, "mcp_x")).unwrap_or_default();
        assert_eq!(v["mcpServers"]["mailcraft"]["args"][2], "--env=MAILCRAFT_MCP_TOKEN=mcp_x");
        let line = code_snippet(&b, "mcp_x");
        assert!(line.contains("-- flatpak run --command=mailcraft-cli --env=MAILCRAFT_MCP_TOKEN=mcp_x io.github.zidan44.MailCraft mcp"), "{line}");
        assert!(!line.contains("{MAILCRAFT"), "{line}");
    }

    #[test]
    fn a_posix_shell_gets_single_quotes_that_nothing_inside_can_break() {
        assert_eq!(quoted_for("/home/a b/x", false), "'/home/a b/x'");
        assert_eq!(quoted_for("/home/$HOME/x", false), "'/home/$HOME/x'");
        assert_eq!(quoted_for("/home/`id`/x", false), "'/home/`id`/x'");
        assert_eq!(quoted_for(r"/home/a\b", false), r"'/home/a\b'");
        assert_eq!(quoted_for("/home/it's/x", false), r"'/home/it'\''s/x'");
        assert_eq!(quoted_for("", false), "''");
        assert_eq!(quoted_for("/usr/bin/cli", false), "/usr/bin/cli");
    }

    /// What a Windows program (Claude Code's `claude.exe`) makes of a command line: the rules of
    /// `CommandLineToArgvW`, for the arguments after the program name.
    fn windows_args(line: &str) -> Vec<String> {
        let (mut args, mut cur, mut quoted, mut started) = (Vec::new(), String::new(), false, false);
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '\\' {
                let n = chars[i..].iter().take_while(|&&c| c == '\\').count();
                i += n;
                started = true;
                if chars.get(i) == Some(&'"') {
                    cur.push_str(&"\\".repeat(n / 2));
                    if n % 2 == 1 {
                        cur.push('"');
                        i += 1;
                    }
                } else {
                    cur.push_str(&"\\".repeat(n));
                }
                continue;
            }
            match c {
                '"' => {
                    quoted = !quoted;
                    started = true;
                }
                ' ' if !quoted => {
                    if started {
                        args.push(std::mem::take(&mut cur));
                        started = false;
                    }
                }
                _ => {
                    cur.push(c);
                    started = true;
                }
            }
            i += 1;
        }
        if started {
            args.push(cur);
        }
        args
    }

    /// The `claude mcp add` line for a Windows path with spaces and backslashes: cmd and
    /// PowerShell both pass a double-quoted word on as one argument, as `claude.exe` reads it
    /// back whole, also for a folder ending in a backslash.
    #[test]
    fn the_windows_claude_code_line_reads_back_as_the_same_arguments() {
        let program = r"C:\Program Files\MailCraft\mailcraft-cli.exe";
        for data in [r"D:\my mail", r"D:\my mail\", r"\\server\share\my mail\", r#"D:\"quoted" mail\"#] {
            let b = bridge(program, &["mcp", "--data", data]);
            let args: Vec<String> = b.args_with_token("mcp_x").iter().map(|a| quoted_for(a, true)).collect();
            let line = format!("{} {}", quoted_for(&b.program, true), args.join(" "));
            let want = vec![program.to_string(), "mcp".into(), "--data".into(), data.to_string()];
            assert_eq!(windows_args(&line), want, "{line}");
        }
        let b = bridge(program, &["mcp"]);
        assert_eq!(quoted_for(&b.program, true), r#""C:\Program Files\MailCraft\mailcraft-cli.exe""#, "double quotes: cmd has no others");
    }

    #[test]
    fn windows_keeps_double_quotes_and_its_backslashes() {
        assert_eq!(quoted_for(r"C:\Mail\cli.exe", true), r"C:\Mail\cli.exe");
        assert_eq!(quoted_for(r"C:\Program Files\cli.exe", true), r#""C:\Program Files\cli.exe""#);
        assert_eq!(quoted_for("a\"b", true), "\"a\\\"b\"");
        assert_eq!(quoted_for("", true), "\"\"");
    }
}
