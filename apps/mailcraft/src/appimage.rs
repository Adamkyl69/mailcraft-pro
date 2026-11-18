//! What MailCraft can tell of the AppImage it may be running from, for the updater
//! (`self_update/detect.rs`) and the Connected apps command (`desktop/start.rs`).

use std::path::{Path, PathBuf};

/// Whether `exe` runs from the AppImage mounted at `appdir`; anything unclear counts as not. An
/// `APPIMAGE` inherited from another program (one AppImage starting another) then doesn't count:
/// the updater treats this copy as what it is (a package, the tarball), and the Connected apps
/// command names the program beside it, never that other program's file.
pub(crate) fn runs_from_image(exe: &Path, appdir: Option<&Path>, canonical: &dyn Fn(&Path) -> Option<PathBuf>) -> bool {
    let Some(appdir) = appdir.filter(|d| d.has_root()).and_then(canonical) else { return false };
    // No image is mounted at the root, where every program would count as inside it.
    appdir.parent().is_some() && exe.starts_with(&appdir)
}
