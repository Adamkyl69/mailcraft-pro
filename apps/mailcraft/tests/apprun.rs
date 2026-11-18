//! The AppImage's entry point (`packaging/linux/AppRun`), run with `sh` over a stand-in AppDir:
//! `mcp` first starts the bridge (`mailcraft-cli`), anything else starts the app, and the
//! arguments reach it as they were given, also through a symlink (as AppImages are started).
#![cfg(unix)]

use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::process::Command;

/// A program that says its name and arguments, one per line.
fn stand_in(path: &Path, name: &str) {
    std::fs::write(path, format!("#!/bin/sh\necho {name}\nfor a in \"$@\"; do echo \"$a\"; done\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn run(app_run: &Path, args: &[&str]) -> Vec<String> {
    let out = Command::new("sh").arg(app_run).args(args).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
}

#[test]
fn app_run_sends_mcp_to_the_bridge_and_everything_else_to_the_app() {
    let dir = std::env::temp_dir().join(format!("mailcraft-apprun-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("usr/bin")).unwrap();
    stand_in(&dir.join("usr/bin/mailcraft"), "app");
    stand_in(&dir.join("usr/bin/mailcraft-cli"), "cli");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/linux/AppRun");
    std::fs::copy(&script, dir.join("AppRun")).unwrap();
    std::fs::set_permissions(dir.join("AppRun"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let app_run = dir.join("AppRun");

    assert_eq!(run(&app_run, &["mcp", "--data", "/a b/c"]), ["cli", "mcp", "--data", "/a b/c"], "the bridge, arguments as given");
    assert_eq!(run(&app_run, &["mcp"]), ["cli", "mcp"]);
    assert_eq!(run(&app_run, &[]), ["app"], "no arguments: the app");
    assert_eq!(run(&app_run, &["--sample", "--cmd", "mail.reply"]), ["app", "--sample", "--cmd", "mail.reply"]);
    assert_eq!(run(&app_run, &["x", "mcp"]), ["app", "x", "mcp"], "only a first argument of mcp");
    assert_eq!(run(&app_run, &["mcpx"]), ["app", "mcpx"]);

    // Started through a link, as `$APPDIR/AppRun` is when it's a symlink or the file is moved.
    let link = std::env::temp_dir().join(format!("mailcraft-apprun-{}-link", std::process::id()));
    let _ = std::fs::remove_file(&link);
    symlink(&app_run, &link).unwrap();
    assert_eq!(run(&link, &["mcp"]), ["cli", "mcp"]);
    let _ = std::fs::remove_file(&link);
    let _ = std::fs::remove_dir_all(&dir);
}
