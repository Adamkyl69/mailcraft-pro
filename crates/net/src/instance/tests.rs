use super::*;

/// Windows names the screen by the session the process runs in, `SESSIONNAME` only when the
/// id can't be read.
#[test]
fn each_windows_session_has_its_own_socket() {
    let name = |id, n| socket_name(Path::new(r"C:\d"), &windows_session(id, n));
    // A launch without SESSIONNAME (Task Scheduler) in the console's session finds the
    // window opened from Explorer there...
    assert_eq!(name(Some(1), None), name(Some(1), Some("Console")));
    // ...and one in a Remote Desktop session never reaches the console's.
    assert_ne!(name(Some(2), None), name(Some(1), Some("Console")));
    assert_ne!(name(Some(2), Some("RDP-Tcp#3")), name(Some(3), Some("RDP-Tcp#3")));
    // Only without an id does SESSIONNAME name it.
    assert_eq!(name(None, Some("Console")), socket_name(Path::new(r"C:\d"), "Console"));
    assert_ne!(name(None, Some("RDP-Tcp#3")), name(None, Some("RDP-Tcp#4")));
    assert_eq!(name(None, None), socket_name(Path::new(r"C:\d"), ""));
}

/// The socket is named by the session this process runs in, which Windows always gives.
#[cfg(windows)]
#[test]
fn this_process_has_a_session_id() {
    let id = session_id();
    assert!(id.is_some(), "the access token names the session");
    assert!(screen().starts_with('s'));
}

/// One data folder, one name; another folder (`--data`, `--sample`, a portable copy) or
/// another screen, another name; never a path.
#[test]
fn each_data_folder_and_screen_has_its_own_socket() {
    let a = socket_name(Path::new("/home/zoe/.local/share/mailcraft"), "wayland-0");
    assert_eq!(a, socket_name(Path::new("/home/zoe/.local/share/mailcraft/"), "wayland-0"));
    assert_ne!(a, socket_name(Path::new("/home/zoe/.local/share/mailcraft/sample"), "wayland-0"));
    assert_ne!(a, socket_name(Path::new("/media/usb/MailCraftData"), "wayland-0"));
    assert_ne!(a, socket_name(Path::new("/home/zoe/.local/share/mailcraft"), "wayland-1"));
    assert_ne!(socket_name(Path::new("/d"), "RDP-Tcp#3"), socket_name(Path::new("/d"), "Console"));
    for odd in ["../../x/y", "RDP-Tcp#3", &"z".repeat(200), ""] {
        let n = socket_name(Path::new("/d"), odd);
        assert!(n.ends_with(".sock") && n.matches('.').count() == 1 && !n.contains(['/', '\\', '#']), "{n}");
        assert!(n.len() <= 16 + 1 + 24 + 5, "{n}");
    }
    #[cfg(windows)]
    assert_eq!(
        socket_name(Path::new(r"C:\Users\Zoe\AppData\Roaming\MailCraft"), ""),
        socket_name(Path::new(r"c:\users\zoe\appdata\roaming\mailcraft"), "")
    );
}
