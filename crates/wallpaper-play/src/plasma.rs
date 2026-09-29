//! KDE Plasma 6 video wallpaper.
//!
//! Plasma 6.7's `plasma-workspace` `wallpapers/` directory ships `color` and
//! `image` (the image package also installs `org.kde.slideshow`). It does not
//! ship a video wallpaper plugin, so this module installs a user-local QML
//! package and selects it through Plasma Shell scripting.
//!
//! The script is evaluated with `qdbus6`, `qdbus-qt6`, or `qdbus`:
//! `org.kde.plasmashell /PlasmaShell org.kde.PlasmaShell.evaluateScript`.
//! `ShellCorona::evaluateScript` runs that script inside plasmashell.
//! `desktops()` is every desktop the shell currently exposes, one per screen
//! and activity, and the script sets the wallpaper plugin on each of them.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use super::{file_url, LaunchError, PlayError, Players};

/// Plugin id installed under the user Plasma wallpaper directory.
///
/// This is not a stock Plasma id. Plasma 6.7 has no video wallpaper plugin.
pub const PLASMA_VIDEO_WALLPAPER_PLUGIN: &str = "linux.wallpaper.video";

/// Session bus service that owns the Plasma shell script engine.
pub const PLASMA_DBUS_SERVICE: &str = "org.kde.plasmashell";

/// Object path of [`PLASMA_DBUS_SERVICE`].
pub const PLASMA_DBUS_PATH: &str = "/PlasmaShell";

/// Method that evaluates a Plasma desktop script.
///
/// Plasma 6 still exposes this as `ShellCorona::evaluateScript`.
pub const PLASMA_DBUS_METHOD: &str = "org.kde.PlasmaShell.evaluateScript";

const PACKAGE: &[(&str, &str)] = &[
    (
        "metadata.json",
        include_str!("../plasma/linux.wallpaper.video/metadata.json"),
    ),
    (
        "contents/config/main.xml",
        include_str!("../plasma/linux.wallpaper.video/contents/config/main.xml"),
    ),
    (
        "contents/ui/main.qml",
        include_str!("../plasma/linux.wallpaper.video/contents/ui/main.qml"),
    ),
    (
        "contents/ui/config.qml",
        include_str!("../plasma/linux.wallpaper.video/contents/ui/config.qml"),
    ),
];

/// `$XDG_DATA_HOME`, or `~/.local/share` when `HOME` is set.
pub(crate) fn default_plasma_data_home() -> PathBuf {
    match env::var_os("XDG_DATA_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => match env::var_os("HOME") {
            Some(home) if !home.is_empty() => PathBuf::from(home).join(".local/share"),
            _ => PathBuf::from(".local/share"),
        },
    }
}

/// First Plasma shell tool found on `PATH`.
///
/// Plasma 6 packages name this program `qdbus6` (Arch, Debian) or
/// `qdbus-qt6` (Fedora `qt6-qttools`). `qdbus` is the older name from KDE's
/// scripting docs. The returned value is the program name, not an absolute
/// path. When none of them is installed the name is `qdbus6`, and spawning it
/// fails with [`LaunchError::MissingPlasmashell`].
pub(crate) fn default_plasmashell_tool() -> PathBuf {
    let path = env::var_os("PATH").unwrap_or_default();
    for name in ["qdbus6", "qdbus-qt6", "qdbus"] {
        for dir in env::split_paths(&path) {
            if dir.join(name).is_file() {
                return PathBuf::from(name);
            }
        }
    }
    PathBuf::from("qdbus6")
}

/// Directory the video wallpaper package is copied into.
pub fn plasma_wallpaper_dir(data_home: &Path) -> PathBuf {
    data_home
        .join("plasma")
        .join("wallpapers")
        .join(PLASMA_VIDEO_WALLPAPER_PLUGIN)
}

/// JavaScript for `org.kde.PlasmaShell.evaluateScript`.
///
/// The script names [`PLASMA_VIDEO_WALLPAPER_PLUGIN`] and the resolved file
/// as a `file://` URL. `muted` is written to the `Muted` config key. The
/// default call passes `true`.
pub fn plasma_wallpaper_script(file: &Path, muted: bool) -> Result<String, PlayError> {
    let url = js_string(&file_url(file)?);
    let plugin = js_string(PLASMA_VIDEO_WALLPAPER_PLUGIN);
    let muted = if muted { "true" } else { "false" };
    Ok(format!(
        "var allDesktops = desktops();\n\
         for (var i = 0; i < allDesktops.length; i++) {{\n\
             var desktop = allDesktops[i];\n\
             desktop.wallpaperPlugin = {plugin};\n\
             desktop.currentConfigGroup = [\"Wallpaper\", {plugin}, \"General\"];\n\
             desktop.writeConfig(\"VideoFile\", {url});\n\
             desktop.writeConfig(\"Muted\", {muted});\n\
             desktop.reloadConfig();\n\
         }}\n"
    ))
}

/// Copy the QML wallpaper package into the user Plasma wallpaper directory.
pub fn install_plasma_video_wallpaper(data_home: &Path) -> Result<PathBuf, LaunchError> {
    let root = plasma_wallpaper_dir(data_home);
    for (relative, contents) in PACKAGE {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| LaunchError::PlasmaInstall {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::write(&path, contents).map_err(|source| LaunchError::PlasmaInstall { path, source })?;
    }
    Ok(root)
}

/// Install the wallpaper package and ask plasmashell to select it.
///
/// This does not spawn the video player. A missing plasmashell tool is
/// [`LaunchError::MissingPlasmashell`].
pub(crate) fn launch_plasma_wallpaper(
    file: &Path,
    players: &Players,
) -> Result<Child, LaunchError> {
    install_plasma_video_wallpaper(&players.plasma_data_home)?;
    let script = plasma_wallpaper_script(file, players.muted).map_err(|error| {
        LaunchError::PlasmaScript {
            message: error.to_string(),
        }
    })?;
    let program = &players.plasmashell;
    Command::new(program)
        .arg(PLASMA_DBUS_SERVICE)
        .arg(PLASMA_DBUS_PATH)
        .arg(PLASMA_DBUS_METHOD)
        .arg(script)
        .spawn()
        .map_err(|source| spawn_error(program, source))
}

fn spawn_error(program: &Path, source: io::Error) -> LaunchError {
    let program = program.to_path_buf();
    if source.kind() == io::ErrorKind::NotFound {
        LaunchError::MissingPlasmashell { program, source }
    } else {
        LaunchError::Spawn { program, source }
    }
}

/// Double-quoted JavaScript string with escapes for quotes and line breaks.
fn js_string(value: &str) -> String {
    let mut out = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}
