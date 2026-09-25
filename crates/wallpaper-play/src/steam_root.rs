//! Locate a Steam installation that contains a Wallpaper Engine workshop.

use std::fmt;
use std::path::{Path, PathBuf};

use wallpaper_import::workshop_dir;

/// Relative Steam roots on Linux, in the order they are searched.
///
/// `~/.steam/steam` is the usual symlink into the real install.
/// `~/.local/share/Steam` is the default data directory. `~/.steam/root` is
/// the other common link to that install.
pub const LINUX_STEAM_ROOTS: [&str; 3] = [".steam/steam", ".local/share/Steam", ".steam/root"];

/// None of the [common Linux Steam roots](LINUX_STEAM_ROOTS) contain a workshop.
#[derive(Debug)]
pub enum SteamRootError {
    /// No candidate under `home` has `steamapps/workshop/content/431960`.
    NotFound { home: PathBuf },
}

impl fmt::Display for SteamRootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SteamRootError::NotFound { home } => write!(
                f,
                "no Steam workshop library under {} (tried .steam/steam, .local/share/Steam, and .steam/root)",
                home.display()
            ),
        }
    }
}

impl std::error::Error for SteamRootError {}

/// First common Steam root under `home` that contains the workshop directory.
///
/// Candidates are [`LINUX_STEAM_ROOTS`], checked in order. A root counts when
/// [`workshop_dir`](wallpaper_import::workshop_dir) finds
/// `steamapps/workshop/content/431960`. A missing root, or a root whose
/// workshop path is absent or not a directory, is skipped. When every
/// candidate is skipped, this returns [`SteamRootError::NotFound`].
pub fn find_steam_root(home: impl AsRef<Path>) -> Result<PathBuf, SteamRootError> {
    let home = home.as_ref();
    for relative in LINUX_STEAM_ROOTS {
        let root = home.join(relative);
        if workshop_dir(&root).is_ok() {
            return Ok(root);
        }
    }
    Err(SteamRootError::NotFound {
        home: home.to_path_buf(),
    })
}
