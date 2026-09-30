//! Library folder, autostart, and update channel choices.
//!
//! The file is `$XDG_CONFIG_HOME/wallpaper/settings.json`, or
//! `~/.config/wallpaper/settings.json` when `XDG_CONFIG_HOME` is unset.
//! Adding a folder stores that path. It does not copy project files.
//! `autostartAsked` is absent until the first-launch question is answered.
//! `updateChannel` is absent while it is still `release`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::Serialize;

/// Which GitHub release stream the desktop checks.
///
/// `release` is the newest non-prerelease tag that starts with `v`.
/// `preview` is the newest prerelease tag that starts with `preview-`.
/// `nightly` is the newest prerelease tag that starts with `nightly-`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpdateChannel {
    #[default]
    Release,
    Preview,
    Nightly,
}

impl UpdateChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            UpdateChannel::Release => "release",
            UpdateChannel::Preview => "preview",
            UpdateChannel::Nightly => "nightly",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "release" => Some(UpdateChannel::Release),
            "preview" => Some(UpdateChannel::Preview),
            "nightly" => Some(UpdateChannel::Nightly),
            _ => None,
        }
    }
}

/// Choices stored in `settings.json`.
///
/// Absent optional fields are omitted from the file. `autostart_asked` is
/// `None` until the user answers the login question. The update channel is
/// omitted while it is [`UpdateChannel::Release`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    pub extra_library: Option<PathBuf>,
    pub autostart_asked: Option<bool>,
    pub autostart: Option<bool>,
    pub update_channel: UpdateChannel,
}

impl Settings {
    /// The first-launch question is still unanswered.
    pub fn needs_autostart_prompt(&self) -> bool {
        self.autostart_asked.is_none()
    }

    /// Login autostart is turned on.
    pub fn autostart_enabled(&self) -> bool {
        self.autostart == Some(true)
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct SettingsFile {
    #[serde(
        rename = "extraLibrary",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    extra_library: Option<String>,
    #[serde(
        rename = "autostartAsked",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    autostart_asked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    autostart: Option<bool>,
    #[serde(
        rename = "updateChannel",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    update_channel: Option<String>,
}

/// `$XDG_CONFIG_HOME`, or `$HOME/.config` when that variable is unset or empty.
pub fn default_config_home() -> PathBuf {
    if let Some(value) = std::env::var_os("XDG_CONFIG_HOME") {
        if !value.is_empty() {
            return PathBuf::from(value);
        }
    }
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".config"),
        None => PathBuf::from(".config"),
    }
}

/// `config_home/wallpaper/settings.json`.
pub fn settings_path(config_home: &Path) -> PathBuf {
    config_home.join("wallpaper").join("settings.json")
}

/// Read settings. A missing file is the default, with every field absent.
pub fn load_settings(config_home: &Path) -> io::Result<Settings> {
    let path = settings_path(config_home);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(Settings::default());
        }
        Err(source) => return Err(source),
    };
    if text.trim().is_empty() {
        return Ok(Settings::default());
    }
    let file: SettingsFile = serde_json::from_str(&text)
        .map_err(|source| io::Error::new(io::ErrorKind::InvalidData, source))?;
    Ok(Settings {
        extra_library: file.extra_library.map(PathBuf::from),
        autostart_asked: file.autostart_asked,
        autostart: file.autostart,
        update_channel: file
            .update_channel
            .as_deref()
            .and_then(UpdateChannel::parse)
            .unwrap_or_default(),
    })
}

/// Write settings. Keys that are still absent are left out of the file.
pub fn save_settings(config_home: &Path, settings: &Settings) -> io::Result<()> {
    let path = settings_path(config_home);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = SettingsFile {
        extra_library: settings
            .extra_library
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
        autostart_asked: settings.autostart_asked,
        autostart: settings.autostart,
        update_channel: match settings.update_channel {
            UpdateChannel::Release => None,
            channel => Some(channel.as_str().to_string()),
        },
    };
    let body = serde_json::to_string(&file)
        .map_err(|source| io::Error::new(io::ErrorKind::InvalidData, source))?;
    fs::write(path, format!("{body}\n"))
}

/// Store one extra library root. The directory must exist. Files stay where they are.
pub fn set_extra_library(config_home: &Path, path: &Path) -> io::Result<Settings> {
    if !path.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("folder is not a directory: {}", path.display()),
        ));
    }
    let mut settings = load_settings(config_home)?;
    settings.extra_library = Some(stored_path(path));
    save_settings(config_home, &settings)?;
    Ok(settings)
}

fn stored_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
    }
}

/// XDG autostart entry. `Exec` runs `wallpaper desktop`.
pub const AUTOSTART_DESKTOP: &str = "\
[Desktop Entry]
Type=Application
Name=Wallpaper
Exec=wallpaper desktop
Terminal=false
X-GNOME-Autostart-enabled=true
";

/// `config_home/autostart/wallpaper.desktop`.
pub fn autostart_desktop_path(config_home: &Path) -> PathBuf {
    config_home.join("autostart").join("wallpaper.desktop")
}

/// Record the login choice and write or remove the autostart desktop file.
///
/// `enabled` writes [`AUTOSTART_DESKTOP`]. A decline, or a later turn off,
/// removes that file when it is present. Either answer sets `autostartAsked`.
pub fn set_autostart(config_home: &Path, enabled: bool) -> io::Result<Settings> {
    let mut settings = load_settings(config_home)?;
    settings.autostart_asked = Some(true);
    settings.autostart = Some(enabled);
    write_autostart_file(config_home, enabled)?;
    save_settings(config_home, &settings)?;
    Ok(settings)
}

/// Store the update channel. `release` is omitted from the file.
pub fn set_update_channel(config_home: &Path, channel: UpdateChannel) -> io::Result<Settings> {
    let mut settings = load_settings(config_home)?;
    settings.update_channel = channel;
    save_settings(config_home, &settings)?;
    Ok(settings)
}

fn write_autostart_file(config_home: &Path, enabled: bool) -> io::Result<()> {
    let path = autostart_desktop_path(config_home);
    if enabled {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, AUTOSTART_DESKTOP)?;
        Ok(())
    } else if path.is_file() {
        fs::remove_file(path)
    } else {
        Ok(())
    }
}
