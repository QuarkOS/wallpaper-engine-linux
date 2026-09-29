//! KDE Plasma 6 video, web, and scene wallpapers.
//!
//! Plasma 6.7's `plasma-workspace` `wallpapers/` directory ships `color` and
//! `image` (the image package also installs `org.kde.slideshow`). It does not
//! ship a video, web, or scene wallpaper plugin, so this module installs
//! user-local QML packages and selects one through Plasma Shell scripting.
//!
//! The video, web, and scene packages are separate directories. Installing
//! one writes only that package's files.
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

use super::{file_url, LaunchError, PlayError, Players, SceneLayer, SceneVisual};

/// Plugin id installed under the user Plasma wallpaper directory.
///
/// This is not a stock Plasma id. Plasma 6.7 has no video wallpaper plugin.
pub const PLASMA_VIDEO_WALLPAPER_PLUGIN: &str = "linux.wallpaper.video";

/// Plugin id for the web wallpaper package.
///
/// This is a different directory from [`PLASMA_VIDEO_WALLPAPER_PLUGIN`].
pub const PLASMA_WEB_WALLPAPER_PLUGIN: &str = "linux.wallpaper.web";

/// Plugin id for the scene wallpaper package.
///
/// Visible image layers are drawn here. This is a different directory from
/// the video and web packages.
pub const PLASMA_SCENE_WALLPAPER_PLUGIN: &str = "linux.wallpaper.scene";

/// Session bus service that owns the Plasma shell script engine.
pub const PLASMA_DBUS_SERVICE: &str = "org.kde.plasmashell";

/// Object path of [`PLASMA_DBUS_SERVICE`].
pub const PLASMA_DBUS_PATH: &str = "/PlasmaShell";

/// Method that evaluates a Plasma desktop script.
///
/// Plasma 6 still exposes this as `ShellCorona::evaluateScript`.
pub const PLASMA_DBUS_METHOD: &str = "org.kde.PlasmaShell.evaluateScript";

const VIDEO_PACKAGE: &[(&str, &str)] = &[
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

const WEB_PACKAGE: &[(&str, &str)] = &[
    (
        "metadata.json",
        include_str!("../plasma/linux.wallpaper.web/metadata.json"),
    ),
    (
        "contents/config/main.xml",
        include_str!("../plasma/linux.wallpaper.web/contents/config/main.xml"),
    ),
    (
        "contents/ui/main.qml",
        include_str!("../plasma/linux.wallpaper.web/contents/ui/main.qml"),
    ),
    (
        "contents/ui/config.qml",
        include_str!("../plasma/linux.wallpaper.web/contents/ui/config.qml"),
    ),
];

const SCENE_PACKAGE: &[(&str, &str)] = &[
    (
        "metadata.json",
        include_str!("../plasma/linux.wallpaper.scene/metadata.json"),
    ),
    (
        "contents/config/main.xml",
        include_str!("../plasma/linux.wallpaper.scene/contents/config/main.xml"),
    ),
    (
        "contents/ui/main.qml",
        include_str!("../plasma/linux.wallpaper.scene/contents/ui/main.qml"),
    ),
    (
        "contents/ui/config.qml",
        include_str!("../plasma/linux.wallpaper.scene/contents/ui/config.qml"),
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
    wallpaper_dir(data_home, PLASMA_VIDEO_WALLPAPER_PLUGIN)
}

/// Directory the web wallpaper package is copied into.
pub fn plasma_web_wallpaper_dir(data_home: &Path) -> PathBuf {
    wallpaper_dir(data_home, PLASMA_WEB_WALLPAPER_PLUGIN)
}

/// Directory the scene wallpaper package is copied into.
pub fn plasma_scene_wallpaper_dir(data_home: &Path) -> PathBuf {
    wallpaper_dir(data_home, PLASMA_SCENE_WALLPAPER_PLUGIN)
}

fn wallpaper_dir(data_home: &Path, plugin: &str) -> PathBuf {
    data_home.join("plasma").join("wallpapers").join(plugin)
}

/// JavaScript for `org.kde.PlasmaShell.evaluateScript`.
///
/// The script names [`PLASMA_VIDEO_WALLPAPER_PLUGIN`] and the resolved file
/// as a `file://` URL. `muted` is written to the `Muted` config key. The
/// default call passes `true`.
pub fn plasma_wallpaper_script(file: &Path, muted: bool) -> Result<String, PlayError> {
    Ok(desktop_script(
        PLASMA_VIDEO_WALLPAPER_PLUGIN,
        "VideoFile",
        &file_url(file)?,
        muted,
    ))
}

/// JavaScript that selects the web wallpaper and loads `url`.
///
/// `url` is the page address, normally a `file://` URL. `muted` is written
/// to the `Muted` config key. The default call passes `true`.
pub fn plasma_web_wallpaper_script(url: &str, muted: bool) -> String {
    desktop_script(PLASMA_WEB_WALLPAPER_PLUGIN, "PageUrl", url, muted)
}

/// JavaScript that selects the scene wallpaper and names every layer.
///
/// `layers` is written as a JSON array of `{url, kind}` objects. Each `url`
/// is a `file://` URL. `kind` is `image` or `video`. The first object is the
/// visual that fills the desktop. `muted` is written to the `Muted` config
/// key and applies to video layers. Still images have no audio. The default
/// call passes `true`.
pub fn plasma_scene_wallpaper_script(layers: &[SceneLayer], muted: bool) -> String {
    let payload =
        serde_json::to_string(&layer_payload(layers)).unwrap_or_else(|_| "[]".to_string());
    desktop_script(PLASMA_SCENE_WALLPAPER_PLUGIN, "Layers", &payload, muted)
}

fn layer_payload(layers: &[SceneLayer]) -> Vec<serde_json::Value> {
    layers
        .iter()
        .map(|layer| {
            serde_json::json!({
                "url": layer.url,
                "kind": match layer.visual {
                    SceneVisual::Image => "image",
                    SceneVisual::Video => "video",
                },
            })
        })
        .collect()
}

fn desktop_script(plugin_id: &str, config_key: &str, config_value: &str, muted: bool) -> String {
    let plugin = js_string(plugin_id);
    let value = js_string(config_value);
    let muted = if muted { "true" } else { "false" };
    format!(
        "var allDesktops = desktops();\n\
         for (var i = 0; i < allDesktops.length; i++) {{\n\
             var desktop = allDesktops[i];\n\
             desktop.wallpaperPlugin = {plugin};\n\
             desktop.currentConfigGroup = [\"Wallpaper\", {plugin}, \"General\"];\n\
             desktop.writeConfig(\"{config_key}\", {value});\n\
             desktop.writeConfig(\"Muted\", {muted});\n\
             desktop.reloadConfig();\n\
         }}\n"
    )
}

/// Copy the video wallpaper package into the user Plasma wallpaper directory.
///
/// The web package, when it is already installed beside this one, is left
/// in place.
pub fn install_plasma_video_wallpaper(data_home: &Path) -> Result<PathBuf, LaunchError> {
    install_wallpaper_package(data_home, PLASMA_VIDEO_WALLPAPER_PLUGIN, VIDEO_PACKAGE)
}

/// Copy the web wallpaper package into the user Plasma wallpaper directory.
///
/// The video package, when it is already installed beside this one, is left
/// in place.
pub fn install_plasma_web_wallpaper(data_home: &Path) -> Result<PathBuf, LaunchError> {
    install_wallpaper_package(data_home, PLASMA_WEB_WALLPAPER_PLUGIN, WEB_PACKAGE)
}

/// Copy the scene wallpaper package into the user Plasma wallpaper directory.
///
/// The video and web packages, when they are already installed beside this
/// one, are left in place.
pub fn install_plasma_scene_wallpaper(data_home: &Path) -> Result<PathBuf, LaunchError> {
    install_wallpaper_package(data_home, PLASMA_SCENE_WALLPAPER_PLUGIN, SCENE_PACKAGE)
}

/// Write one wallpaper package. Sibling packages under `wallpapers/` stay.
fn install_wallpaper_package(
    data_home: &Path,
    plugin: &str,
    package: &[(&str, &str)],
) -> Result<PathBuf, LaunchError> {
    let root = wallpaper_dir(data_home, plugin);
    for (relative, contents) in package {
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

/// Install the video wallpaper package and ask plasmashell to select it.
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
    evaluate_plasma_script(&players.plasmashell, &script)
}

/// Install the web wallpaper package and ask plasmashell to select it.
///
/// This does not spawn [`Players::web`]. A missing plasmashell tool is
/// [`LaunchError::MissingPlasmashell`].
pub(crate) fn launch_plasma_web_wallpaper(
    url: &str,
    players: &Players,
) -> Result<Child, LaunchError> {
    install_plasma_web_wallpaper(&players.plasma_data_home)?;
    let script = plasma_web_wallpaper_script(url, players.muted);
    evaluate_plasma_script(&players.plasmashell, &script)
}

/// Install the scene wallpaper package and ask plasmashell to select it.
///
/// This does not spawn a video player or a web browser, including when
/// [`Players::plasma`] or [`Players::web_plasma`] is clear. A missing
/// plasmashell tool is [`LaunchError::MissingPlasmashell`]. An empty layer
/// list is refused before anything is installed.
pub(crate) fn launch_plasma_scene_wallpaper(
    layers: &[SceneLayer],
    players: &Players,
) -> Result<Child, LaunchError> {
    if layers.is_empty() {
        return Err(LaunchError::PlasmaScript {
            message: "wallpaper type `scene` cannot be played".to_string(),
        });
    }
    install_plasma_scene_wallpaper(&players.plasma_data_home)?;
    let script = plasma_scene_wallpaper_script(layers, players.muted);
    evaluate_plasma_script(&players.plasmashell, &script)
}

fn evaluate_plasma_script(program: &Path, script: &str) -> Result<Child, LaunchError> {
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
