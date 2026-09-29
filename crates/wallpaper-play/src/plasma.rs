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

/// Installed Wallpaper Engine for KDE plugin.
///
/// `KPlugin.Id` in
/// <https://github.com/CaptSilver/wallpaper-engine-kde-plugin/blob/main/plugin/metadata.json.in>.
/// CMake installs that package at
/// `plasma/wallpapers/com.github.captsilver.wallpaperEngineKde`
/// (<https://github.com/CaptSilver/wallpaper-engine-kde-plugin/blob/main/CMakeLists.txt>).
pub const LIVE_SCENE_PLUGIN_ID: &str = "com.github.captsilver.wallpaperEngineKde";

/// Config key for the selected wallpaper project.
///
/// `WallpaperSource` in
/// <https://github.com/CaptSilver/wallpaper-engine-kde-plugin/blob/main/plugin/contents/config/main.xml>.
/// The plugin stores `projectDir/file+type` (`packWallpaperSource` in
/// `plugin/contents/ui/Common.qml` of that repository).
pub const LIVE_SCENE_SOURCE_KEY: &str = "WallpaperSource";

/// Workshop item id written with [`LIVE_SCENE_SOURCE_KEY`].
///
/// `WallpaperWorkShopId` in the same `main.xml`. The runtime sets both when
/// a wallpaper is chosen (`setWallpaperFromItem` in
/// `plugin/contents/ui/main.qml`).
pub const LIVE_SCENE_WORKSHOP_ID_KEY: &str = "WallpaperWorkShopId";

/// Mute flag for the live plugin. `MuteAudio` in the same `main.xml`.
pub const LIVE_SCENE_MUTE_KEY: &str = "MuteAudio";

/// Project that provides [`LIVE_SCENE_PLUGIN_ID`].
pub const LIVE_SCENE_UPSTREAM_URL: &str =
    "https://github.com/CaptSilver/wallpaper-engine-kde-plugin";

/// Colon-separated data roots that replace the default Plasma wallpaper search.
///
/// When this is set, [`live_scene_data_dirs`] does not read `/usr/share`,
/// `$XDG_DATA_HOME`, or `$XDG_DATA_DIRS`. An empty value searches nowhere.
pub const LIVE_SCENE_DATA_DIRS_ENV: &str = "WALLPAPER_PLASMA_DATA_DIRS";

/// Sentence printed when a scene plays through the static image plugin.
pub const PARTIAL_SCENE_NOTICE: &str = concat!(
    "this scene is a partial view of its image and video layers. ",
    "Particles, 3D models, and user scripts stay unsupported. ",
    "https://github.com/CaptSilver/wallpaper-engine-kde-plugin"
);

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

/// Data directories searched for [`LIVE_SCENE_PLUGIN_ID`].
///
/// [`LIVE_SCENE_DATA_DIRS_ENV`], when set, is the whole list. Otherwise the
/// list is `/usr/share`, `$XDG_DATA_HOME` (or `~/.local/share`), and each
/// entry of `$XDG_DATA_DIRS` (or `/usr/local/share` and `/usr/share`).
pub fn live_scene_data_dirs() -> Vec<PathBuf> {
    let override_dirs = env::var_os(LIVE_SCENE_DATA_DIRS_ENV);
    let data_home = xdg_data_home();
    let extra = env::var_os("XDG_DATA_DIRS");
    live_scene_data_dirs_from(
        override_dirs.as_deref(),
        data_home.as_deref(),
        extra.as_deref(),
    )
}

fn xdg_data_home() -> Option<PathBuf> {
    match env::var_os("XDG_DATA_HOME") {
        Some(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => {
            let home = env::var_os("HOME")?;
            if home.is_empty() {
                None
            } else {
                Some(PathBuf::from(home).join(".local/share"))
            }
        }
    }
}

/// Build the live-plugin search list from already-resolved inputs.
///
/// `override_dirs` replaces every other root. `data_home` is `$XDG_DATA_HOME`.
/// `extra_data_dirs` is `$XDG_DATA_DIRS`, colon-separated. `None` for the
/// extra list uses `/usr/local/share` and `/usr/share`.
pub fn live_scene_data_dirs_from(
    override_dirs: Option<&std::ffi::OsStr>,
    data_home: Option<&Path>,
    extra_data_dirs: Option<&std::ffi::OsStr>,
) -> Vec<PathBuf> {
    if let Some(override_dirs) = override_dirs {
        return split_data_dirs(override_dirs);
    }
    let mut dirs = vec![PathBuf::from("/usr/share")];
    if let Some(data_home) = data_home {
        if !data_home.as_os_str().is_empty() {
            dirs.push(data_home.to_path_buf());
        }
    }
    match extra_data_dirs {
        Some(value) if !value.is_empty() => dirs.extend(split_data_dirs(value)),
        _ => {
            dirs.push(PathBuf::from("/usr/local/share"));
            dirs.push(PathBuf::from("/usr/share"));
        }
    }
    dirs
}

fn split_data_dirs(value: &std::ffi::OsStr) -> Vec<PathBuf> {
    env::split_paths(value)
        .filter(|path| !path.as_os_str().is_empty())
        .collect()
}

/// The live scene package directory exists under one of `data_dirs`.
///
/// Each root is a Plasma data directory. The package is
/// `plasma/wallpapers/` joined with [`LIVE_SCENE_PLUGIN_ID`].
pub fn live_scene_plugin_installed_in(data_dirs: &[PathBuf]) -> bool {
    data_dirs
        .iter()
        .any(|root| live_scene_package_dir(root).is_dir())
}

/// [`live_scene_plugin_installed_in`] for [`live_scene_data_dirs`].
pub fn live_scene_plugin_installed() -> bool {
    live_scene_plugin_installed_in(&live_scene_data_dirs())
}

fn live_scene_package_dir(data_dir: &Path) -> PathBuf {
    data_dir
        .join("plasma")
        .join("wallpapers")
        .join(LIVE_SCENE_PLUGIN_ID)
}

/// JavaScript that selects the installed live scene plugin.
///
/// The script names [`LIVE_SCENE_PLUGIN_ID`]. [`LIVE_SCENE_SOURCE_KEY`] is
/// `directory/file+scene`, which contains the workshop project directory.
/// [`LIVE_SCENE_WORKSHOP_ID_KEY`] is that directory's name. `muted` is
/// written to [`LIVE_SCENE_MUTE_KEY`].
pub fn live_scene_wallpaper_script(directory: &Path, muted: bool) -> String {
    let plugin = js_string(LIVE_SCENE_PLUGIN_ID);
    let source = js_string(&live_scene_source_value(directory));
    let workshop_id = js_string(&workshop_id(directory));
    let muted = if muted { "true" } else { "false" };
    format!(
        "var allDesktops = desktops();\n\
         for (var i = 0; i < allDesktops.length; i++) {{\n\
             var desktop = allDesktops[i];\n\
             desktop.wallpaperPlugin = {plugin};\n\
             desktop.currentConfigGroup = [\"Wallpaper\", {plugin}, \"General\"];\n\
             desktop.writeConfig(\"{LIVE_SCENE_SOURCE_KEY}\", {source});\n\
             desktop.writeConfig(\"{LIVE_SCENE_WORKSHOP_ID_KEY}\", {workshop_id});\n\
             desktop.writeConfig(\"{LIVE_SCENE_MUTE_KEY}\", {muted});\n\
             desktop.reloadConfig();\n\
         }}\n"
    )
}

/// `projectDir/file+scene`, the value the live plugin stores for one project.
fn live_scene_source_value(directory: &Path) -> String {
    let file = project_relative_file(directory);
    format!("{}/{file}+scene", directory.display())
}

fn workshop_id(directory: &Path) -> String {
    directory
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `project.json` `file`, or `scene.json` when that field is missing.
fn project_relative_file(directory: &Path) -> String {
    let Ok(text) = fs::read_to_string(directory.join("project.json")) else {
        return "scene.json".to_string();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return "scene.json".to_string();
    };
    value
        .get("file")
        .and_then(|file| file.as_str())
        .map(str::trim)
        .filter(|file| !file.is_empty())
        .unwrap_or("scene.json")
        .to_string()
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

/// Ask plasmashell to select the installed live scene plugin.
///
/// `directory` is the workshop project directory. This does not install
/// [`PLASMA_SCENE_WALLPAPER_PLUGIN`] and does not spawn a video player or a
/// web browser. A missing plasmashell tool is
/// [`LaunchError::MissingPlasmashell`].
pub(crate) fn launch_live_scene(directory: &Path, players: &Players) -> Result<Child, LaunchError> {
    let script = live_scene_wallpaper_script(directory, players.muted);
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
