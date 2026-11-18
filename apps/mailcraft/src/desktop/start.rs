//! What MailCraft starts with: its session, made on the `data-open` thread once the data files
//! are open (`boot.rs`), and the `--cmd` commands run at start-up.

use mailcraft_ui_egui::backstage::{Backstage, Page};

use super::*;

/// What a session is made with, besides its store.
#[derive(Clone)]
pub(crate) struct Made {
    pub(crate) clock: Clock,
    /// MailCraft is portable (`portable.rs`): passwords and sign-ins are kept in memory only.
    pub(crate) portable: bool,
    /// A release build's commit, so File › About can tell its test build from a later one.
    pub(crate) build_sha: Option<String>,
    /// How AI apps start the `mailcraft-cli mcp` bridge, for File › Options › Connected apps.
    pub(crate) mcp_bridge: Option<mailcraft_engine::mcp::BridgeCommand>,
}

/// The system's secure random bytes, for MCP tokens and the authenticator's secret.
fn random(n: usize) -> Option<Vec<u8>> {
    mailcraft_net::oauth::random_bytes(n).ok()
}

/// How this MailCraft was installed, as far as the bridge's command depends on it (from the
/// environment in `main.rs`).
#[derive(Clone, Copy, Default)]
pub(crate) struct Packaging<'a> {
    /// `$APPIMAGE`, set by an AppImage's runtime to the file itself.
    pub(crate) appimage: Option<&'a std::path::Path>,
    /// `$APPDIR`, the folder the AppImage is mounted at.
    pub(crate) appdir: Option<&'a std::path::Path>,
    /// The app's id in a Flatpak sandbox ([`mailcraft_engine::store::flatpak_app_id`]).
    pub(crate) flatpak_id: Option<&'a str>,
    /// A macOS build.
    pub(crate) macos: bool,
}

/// How an AI app starts the bridge: in a Flatpak (`flatpak_id` is `$FLATPAK_ID`), `flatpak run
/// --command=mailcraft-cli <id> mcp` on the host, since the program inside the sandbox can't be
/// run from outside it (`flatpak run` passes the caller's environment on, so the token stays in
/// the snippet's environment as everywhere); the AppImage file itself when MailCraft runs from one
/// (`appimage` is `$APPIMAGE`: its `AppRun` takes `mcp` as the first argument, and the file keeps
/// its path where the temporary mount the running program is in changes on every launch); else
/// `mailcraft-cli` beside this program when it's there, else by name (found on the path), with
/// `--data` when this MailCraft's data folder isn't the default one (`pass_data`: it was chosen
/// at start, or MailCraft is portable). Shown on screen only, never written to the log or a file.
///
/// `None` on macOS when `mailcraft-cli` isn't beside this program (its dmg holds the app alone,
/// the CLI is a zip of its own): the card says so rather than showing a snippet that won't run
/// (`docs/gaps.md`).
pub(crate) fn bridge_command(
    exe: Option<&std::path::Path>,
    root: Option<&std::path::Path>,
    pass_data: bool,
    packaging: &Packaging,
) -> Option<mailcraft_engine::mcp::BridgeCommand> {
    use mailcraft_engine::mcp::BridgeCommand;
    let Packaging { appimage, appdir, flatpak_id, macos } = *packaging;
    let mut args = vec!["mcp".to_string()];
    if let Some(root) = root.filter(|_| pass_data) {
        args.push("--data".into());
        args.push(root.to_string_lossy().into_owned());
    }
    if let Some(id) = flatpak_id {
        let head = ["run", "--command=mailcraft-cli", id].map(str::to_string);
        return Some(BridgeCommand { program: "flatpak".into(), args: head.into_iter().chain(args).collect() });
    }
    let name = if cfg!(windows) { "mailcraft-cli.exe" } else { "mailcraft-cli" };
    let beside = exe.and_then(std::path::Path::parent).map(|dir| dir.join(name)).filter(|p| p.is_file());
    // `$APPIMAGE` is passed on by an AppImage to the programs it starts (a browser's `mailto:`
    // link), so it is trusted only for a MailCraft that runs from inside its own image.
    let appimage = appimage.filter(|p| {
        p.is_absolute() && p.is_file() && exe.is_some_and(|e| crate::appimage::runs_from_image(e, appdir, &|d| std::fs::canonicalize(d).ok()))
    });
    if macos && appimage.is_none() && beside.is_none() {
        return None;
    }
    let program = appimage.or(beside.as_deref()).map_or_else(|| "mailcraft-cli".to_string(), |p| p.to_string_lossy().into_owned());
    Some(BridgeCommand { program, args })
}

/// The session over `store`, as MailCraft opens with it (made on the `data-open` thread).
pub(crate) fn session(store: Option<Store>, Made { clock, portable, build_sha, mcp_bridge }: Made) -> Session {
    let mut session = Session::open(store, clock);
    session.oauth_available = mailcraft_net::oauth::available();
    // S/MIME signers are trusted through the roots this computer trusts.
    session.set_trust_roots(mailcraft_net::trust_roots());
    session.set_card_modules(mailcraft_net::card_modules());
    session.portable = portable;
    session.build_sha = build_sha;
    // A build of a release checks for releases only (File › About too).
    session.updates_stable_only = crate::self_update::stable();
    // What connected AI apps (MCP) need from this computer: secure random bytes, this run's log,
    // and how to start the bridge; and the approval secret, read from the keychain.
    session.mcp.random = Some(random);
    session.mcp.log_source = Some(crate::startup::logfile::recent);
    session.mcp.bridge = mcp_bridge;
    if session.mcp_wants_secret() {
        session.push_job(mailcraft_engine::model::Job::ReadAppSecret { what: mailcraft_engine::mcp::APPROVAL_SECRET.to_string() });
    }
    session
}

/// Run the `--cmd` commands MailCraft was started with (`ui.*` ones open that page or dialog).
/// Params that aren't JSON are said, and that command isn't run, whichever it is.
pub(crate) fn commands(app: &mut MailApp, args: &[String]) {
    for c in args.windows(2).filter(|w| w.first().is_some_and(|a| a == "--cmd")).filter_map(|w| w.get(1)) {
        let (id, params) = c.split_once('=').unwrap_or((c.as_str(), "{}"));
        let p: serde_json::Value = match serde_json::from_str(params) {
            Ok(p) => p,
            Err(e) => {
                say(&format!("--cmd {id}: bad JSON: {e}"));
                continue;
            }
        };
        match id {
            "ui.addAccount" => {
                let mut d = mailcraft_ui_egui::Dialog::add_account_with(
                    p.get("email").and_then(|v| v.as_str()).unwrap_or_default(),
                    p.get("advanced").and_then(|v| v.as_bool()).unwrap_or(false),
                );
                if let mailcraft_ui_egui::Dialog::AddAccount(f) = &mut d {
                    let flag = |name: &str| p.get(name).and_then(|v| v.as_bool()).unwrap_or(false);
                    if flag("graph") {
                        f.kind = mailcraft_ui_egui::AccountKind::Graph;
                    } else if flag("pop") {
                        f.kind = mailcraft_ui_egui::AccountKind::Pop;
                    }
                    f.calendar_only = flag("calendarOnly");
                }
                app.ui.dialog = Some(d);
            }
            "ui.file" | "ui.settings" => app.ui.backstage = Some(Backstage::new(Page::Info)),
            "ui.options" => app.ui.backstage = Some(Backstage::new(Page::Options)),
            "ui.openExport" => app.ui.backstage = Some(Backstage::new(Page::Open)),
            "ui.about" => app.ui.backstage = Some(Backstage::new(Page::About)),
            "ui.newEvent" => app.ui.dialog = Some(mailcraft_ui_egui::Dialog::new_event(&app.session, None)),
            "ui.newContact" => app.ui.dialog = Some(mailcraft_ui_egui::Dialog::new_contact()),
            "ui.newTask" => app.ui.dialog = Some(mailcraft_ui_egui::Dialog::new_task(&app.session, None)),
            // A task's form, by its uid (`{"uid": …}`).
            "ui.openTask" => {
                app.ui.dialog = mailcraft_ui_egui::Dialog::open_task(&app.session, p.get("uid").and_then(|v| v.as_str()).unwrap_or_default());
            }
            "ui.signature" => app.ui.dialog = mailcraft_ui_egui::Dialog::signature(&app.session),
            "ui.categories" => app.ui.dialog = Some(mailcraft_ui_egui::Dialog::Categories),
            "ui.nameKeyword" => {
                let k = p.get("keyword").and_then(|v| v.as_str()).unwrap_or_default();
                app.ui.dialog = Some(mailcraft_ui_egui::Dialog::name_keyword(k));
            }
            _ => app.run(id, p),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> MailApp {
        let session = Session::open(None, Clock::new(1_700_000_000, 0));
        MailApp::new(session, Box::new(mailcraft_ui_egui::NullBackend), Box::new(mailcraft_ui_egui::NullServices(1_700_000_000)))
    }

    /// The bridge is started as `mailcraft-cli` beside the app when it's there, else by name;
    /// `--data` follows only when the data folder isn't the default one.
    #[test]
    fn the_bridge_command_is_worked_out_for_the_snippets() {
        let dir = std::env::temp_dir().join(format!("mailcraft-start-{}-bridge", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        let exe = dir.join("mailcraft");
        let root = dir.join("data");
        let by_name = |args: &[&str]| mailcraft_engine::mcp::BridgeCommand {
            program: "mailcraft-cli".into(),
            args: args.iter().map(|a| a.to_string()).collect(),
        };
        assert_eq!(bc(Some(&exe), Some(&root), false, None, None, None), by_name(&["mcp"]), "none beside it: found by name");
        assert_eq!(bc(None, None, true, None, None, None), by_name(&["mcp"]), "no folder to name");
        let cli = dir.join(if cfg!(windows) { "mailcraft-cli.exe" } else { "mailcraft-cli" });
        std::fs::write(&cli, b"").unwrap();
        let with = bc(Some(&exe), Some(&root), true, None, None, None);
        assert_eq!(with.program, cli.to_string_lossy());
        assert_eq!(with.args, ["mcp".to_string(), "--data".into(), root.to_string_lossy().into_owned()]);
        assert_eq!(bc(Some(&exe), Some(&root), false, None, None, None).args, ["mcp"], "the default folder needs none");

        // From an AppImage: the file itself (its AppRun takes `mcp`), not the temporary mount's
        // copy beside the running program.
        let image = dir.join("MailCraft.AppImage");
        std::fs::write(&image, b"").unwrap();
        let from_image = bc(Some(&exe), Some(&root), true, Some(&image), Some(&dir), None);
        assert_eq!(from_image.program, image.to_string_lossy());
        assert_eq!(from_image.args, with.args, "the --data rule is the same");
        assert_eq!(bc(Some(&exe), None, false, Some(&image), Some(&dir), None).args, ["mcp"]);
        let gone = dir.join("moved.AppImage");
        assert_eq!(bc(Some(&exe), None, false, Some(&gone), Some(&dir), None).program, cli.to_string_lossy(), "a file that isn't there is not used");
        assert_eq!(
            bc(None, None, false, Some(std::path::Path::new("MailCraft.AppImage")), Some(&dir), None),
            by_name(&["mcp"]),
            "a relative path would depend on where the app runs"
        );

        // An AppImage's `$APPIMAGE` is passed on to what it starts: a MailCraft that is not inside
        // that image (a package's, started by another AppImage) doesn't use it.
        let other = dir.join("other");
        std::fs::create_dir_all(&other).unwrap();
        let elsewhere = bc(Some(&exe), Some(&root), true, Some(&image), Some(&other), None);
        assert_eq!(elsewhere.program, cli.to_string_lossy(), "outside that image: the CLI beside it");
        assert_eq!(bc(Some(&empty_beside(&dir)), None, false, Some(&image), Some(&other), None), by_name(&["mcp"]), "or the bare name");
        assert_eq!(bc(Some(&exe), None, false, Some(&image), None, None).program, cli.to_string_lossy(), "no APPDIR: not in an image");
        assert_eq!(bc(None, None, false, Some(&image), Some(&dir), None), by_name(&["mcp"]), "no program path: not in an image");
        assert_eq!(
            bc(Some(&exe), None, false, Some(&image), Some(std::path::Path::new("/")), None).program,
            cli.to_string_lossy(),
            "no image is mounted at /"
        );

        // In a Flatpak: the host runs it through flatpak, whatever is beside the program.
        let id = "io.github.zidan44.MailCraft";
        let sandboxed = bc(Some(&exe), Some(&root), false, None, None, Some(id));
        assert_eq!(sandboxed.program, "flatpak");
        assert_eq!(sandboxed.args, ["run", "--command=mailcraft-cli", id, "mcp"]);
        let with_data = bc(None, Some(&root), true, Some(&image), Some(&dir), Some(id));
        assert_eq!(with_data.args[3..], ["mcp".to_string(), "--data".into(), root.to_string_lossy().into_owned()], "the --data rule is the same");

        // macOS: the CLI is not in the dmg, so no command unless it is beside the app (or the
        // app is run from a place that has it).
        let mac = |exe: Option<&std::path::Path>| {
            bridge_command(exe, None, false, &Packaging { appimage: None, appdir: None, flatpak_id: None, macos: true })
        };
        assert_eq!(mac(Some(&exe)).map(|b| b.program), Some(cli.to_string_lossy().into_owned()), "beside the app: as on every system");
        let empty = dir.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(mac(Some(&empty.join("MailCraft"))), None, "not packaged: no snippet");
        assert_eq!(mac(None), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn bc(
        exe: Option<&std::path::Path>,
        root: Option<&std::path::Path>,
        pass_data: bool,
        appimage: Option<&std::path::Path>,
        appdir: Option<&std::path::Path>,
        flatpak_id: Option<&str>,
    ) -> mailcraft_engine::mcp::BridgeCommand {
        bridge_command(exe, root, pass_data, &Packaging { appimage, appdir, flatpak_id, macos: false }).unwrap()
    }

    /// A program path in a folder with no `mailcraft-cli` beside it.
    fn empty_beside(dir: &std::path::Path) -> std::path::PathBuf {
        let empty = dir.join("empty-beside");
        std::fs::create_dir_all(&empty).unwrap();
        empty.join("mailcraft")
    }

    /// The session carries what MCP needs of this computer: random bytes, the log, the bridge.
    #[test]
    fn the_session_is_given_what_mcp_needs_of_the_computer() {
        let bridge = mailcraft_engine::mcp::BridgeCommand { program: "mailcraft-cli".into(), args: vec!["mcp".into()] };
        let made = Made { clock: Clock::new(1_700_000_000, 0), portable: false, build_sha: None, mcp_bridge: Some(bridge.clone()) };
        let s = session(None, made);
        assert_eq!(s.mcp.bridge, Some(bridge));
        assert_eq!(s.mcp.random.and_then(|r| r(20)).map(|b| b.len()), Some(20));
        assert!(s.mcp.log_source.is_some());
    }

    fn cmd(app: &mut MailApp, c: &str) {
        commands(app, &["--cmd".to_string(), c.to_string()]);
    }

    /// Found in audit (#232): `--cmd ui.addAccount={bad` opened the dialog empty without a word,
    /// where `--cmd mail.new={bad` said its JSON was bad. Bad JSON is said for every command,
    /// and that one isn't run; good JSON, or none, is as before.
    #[test]
    fn bad_json_is_refused_for_every_command() {
        let mut a = app();
        for bad in ["ui.addAccount={bad", "ui.openTask={\"uid\":", "ui.nameKeyword=x", "ui.about=", "mail.new={bad"] {
            cmd(&mut a, bad);
            assert!(a.ui.dialog.is_none() && a.ui.backstage.is_none(), "{bad} was run");
        }
        cmd(&mut a, "ui.addAccount={\"email\":\"zoe@example.com\",\"pop\":true}");
        let Some(mailcraft_ui_egui::Dialog::AddAccount(f)) = &a.ui.dialog else { panic!("no Add account dialog") };
        assert_eq!(f.kind, mailcraft_ui_egui::AccountKind::Pop);
        cmd(&mut a, "ui.about");
        assert!(a.ui.backstage.is_some(), "no params: as `{{}}`");
    }
}
