//! Turn a resolved video, web, or scene wallpaper into a play plan and start it.
//!
//! A desktop player scans a library with [`wallpaper_import::scan_library`],
//! then calls [`plan_playback`] on each entry. [`launch`] starts that plan.
//! A video plan is a muted KDE Plasma wallpaper: a user-local QML plugin
//! loops the file with Qt Multimedia, and Plasma Shell scripting selects that
//! plugin on every desktop. Pass an explicit video player to spawn `mpv`
//! instead. A web plan is a muted KDE Plasma wallpaper too: a separate
//! user-local QML plugin loads the page in Qt WebEngine. Pass an explicit
//! web player to spawn that program with the `file://` URL instead. A scene
//! plan installs `linux.wallpaper.scene` and shows visible image layers. A
//! video texture loops muted. A still image has no audio. Particles, text,
//! models, and scripts are not played. [`play_entry`] selects an installed
//! Wallpaper Engine for KDE plugin for a scene and passes the workshop
//! project directory. When that plugin is absent, a scene with an image or
//! video layer stays on `linux.wallpaper.scene`. Application projects, a
//! scene with nothing to show, other unsupported types, and video or web
//! projects whose media file is missing, are errors from [`plan_playback`].
//! [`play_first`] scans a library, skips unsupported entries (including
//! scenes), and [`launch`]es the first video or web wallpaper.
//!
//! The `wallpaper` command calls [`play_first`] on a workshop library, or
//! [`play_entry`] when an id is given. `wallpaper ui` serves a local page
//! that lists the same library and plays one video or web item. A scene is
//! offered on that page when the live plugin is installed. A scene with
//! nothing to show, and no live plugin, is refused and does not start a player.
//! `wallpaper play --steam-root DIR` uses that Steam root, then any extra
//! library named in its `steamapps/libraryfolders.vdf`. Without
//! `--steam-root`, [`steam_root::find_steam_root`] checks `~/.steam/steam`,
//! `~/.local/share/Steam`, and `~/.steam/root` under `HOME`, in that order.
//! Each root is used when its workshop directory exists. Otherwise libraries
//! named in that root's `libraryfolders.vdf` are checked, and the first match
//! is used.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use wallpaper_import::{resolve_media, scan_library, ImportError, LibraryEntry, Wallpaper};

mod plasma;
mod scene;
pub mod steam_root;
pub mod ui;

pub use plasma::{
    install_plasma_scene_wallpaper, install_plasma_video_wallpaper, install_plasma_web_wallpaper,
    launch_live_scene, live_scene_data_dirs, live_scene_data_dirs_from,
    live_scene_plugin_installed, live_scene_plugin_installed_in, live_scene_wallpaper_script,
    plasma_scene_wallpaper_dir, plasma_scene_wallpaper_script, plasma_wallpaper_dir,
    plasma_wallpaper_script, plasma_web_wallpaper_dir, plasma_web_wallpaper_script,
    LIVE_SCENE_DATA_DIRS_ENV, LIVE_SCENE_MUTE_KEY, LIVE_SCENE_PLUGIN_ID, LIVE_SCENE_SOURCE_KEY,
    LIVE_SCENE_UPSTREAM_URL, LIVE_SCENE_WORKSHOP_ID_KEY, PARTIAL_SCENE_NOTICE, PLASMA_DBUS_METHOD,
    PLASMA_DBUS_PATH, PLASMA_DBUS_SERVICE, PLASMA_SCENE_WALLPAPER_PLUGIN,
    PLASMA_VIDEO_WALLPAPER_PLUGIN, PLASMA_WEB_WALLPAPER_PLUGIN,
};

/// One visible image layer resolved from a scene.
///
/// `url` is a `file://` URL of a file inside the project directory.
/// [`SceneVisual::Image`] is a still picture and has no audio.
/// [`SceneVisual::Video`] loops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneLayer {
    pub url: String,
    pub visual: SceneVisual,
}

/// Picture or looping video inside a [`SceneLayer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneVisual {
    /// png, jpg, jpeg, gif, or webp. No audio output.
    Image,
    /// mp4, webm, or mkv. Loops. Muted unless sound was requested.
    Video,
}

/// What a desktop player should run for one library entry.
///
/// Video playback loops. Web playback loads the HTML file through a `file://`
/// URL. Scene playback shows the resolved image layers. The player does not
/// choose a different file than the one
/// [`resolve_media`](wallpaper_import::resolve_media) returned for video and
/// web, or the scene resolver returned for an image layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayPlan {
    /// Open `file` and play it again from the start when it ends.
    ///
    /// `loops` is true. `file` is the resolved media path.
    Video { file: PathBuf, loops: bool },
    /// Navigate a web view to this `file://` URL.
    ///
    /// The URL points at the resolved HTML file.
    Web { url: String },
    /// Show scene image layers on the desktop.
    ///
    /// `layers` is in scene order. The first entry fills the desktop. Later
    /// entries are shown as well. [`plan_playback`] does not produce an empty
    /// list.
    Scene { layers: Vec<SceneLayer> },
}

/// Failure while turning a library entry into a [`PlayPlan`].
#[derive(Debug)]
pub enum PlayError {
    /// A scene with nothing to show, an application, or another type this
    /// player does not play.
    Unsupported { kind: String },
    /// The project names a media file that is not a file on disk.
    MissingFile { path: PathBuf },
    /// `file` is absolute or leaves the project directory.
    PathEscapes { directory: PathBuf, file: String },
    /// Reading the project directory or the media file failed.
    Io { path: PathBuf, source: io::Error },
    /// Resolving media failed for a reason other than the cases above.
    Resolve(ImportError),
}

impl fmt::Display for PlayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlayError::Unsupported { kind } => {
                write!(f, "wallpaper type `{kind}` cannot be played")
            }
            PlayError::MissingFile { path } => {
                write!(f, "media file not found: {}", path.display())
            }
            PlayError::PathEscapes { directory, file } => {
                write!(
                    f,
                    "media path `{file}` escapes project directory {}",
                    directory.display()
                )
            }
            PlayError::Io { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
            PlayError::Resolve(source) => write!(f, "{source}"),
        }
    }
}

impl std::error::Error for PlayError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PlayError::Io { source, .. } => Some(source),
            PlayError::Resolve(source) => Some(source),
            _ => None,
        }
    }
}

/// Plan how a desktop player should play one library entry.
///
/// Video and web entries must resolve to a file inside the project directory.
/// The video plan names that file and sets `loops` so the player repeats it.
/// The web plan is a `file://` URL for that HTML file.
///
/// A scene entry plays visible image layers from its scene file. Each layer's
/// image path is a picture, a video, or a model JSON whose material names a
/// texture. Particles, text, models, scripts, and hidden layers are skipped.
/// A scene with nothing to show returns [`PlayError::Unsupported`] and does
/// not name a file. Other unsupported entries and missing video or web files
/// return an error instead of a plan.
pub fn plan_playback(entry: &LibraryEntry) -> Result<PlayPlan, PlayError> {
    match &entry.wallpaper {
        Wallpaper::Unsupported { kind } if is_scene(kind) => plan_scene(entry, kind),
        Wallpaper::Unsupported { kind } => Err(PlayError::Unsupported { kind: kind.clone() }),
        Wallpaper::Video { .. } => {
            let file = resolve_media(entry).map_err(from_import)?;
            Ok(PlayPlan::Video { file, loops: true })
        }
        Wallpaper::Web { .. } => {
            let file = resolve_media(entry).map_err(from_import)?;
            Ok(PlayPlan::Web {
                url: file_url(&file)?,
            })
        }
    }
}

fn is_scene(kind: &str) -> bool {
    kind.eq_ignore_ascii_case("scene")
}

/// Scene failures stay [`PlayError::Unsupported`]. The message does not name
/// a path, including when the project directory is missing.
fn plan_scene(entry: &LibraryEntry, kind: &str) -> Result<PlayPlan, PlayError> {
    let layers = scene::scene_layers(&entry.directory);
    if layers.is_empty() {
        Err(PlayError::Unsupported {
            kind: kind.to_string(),
        })
    } else {
        Ok(PlayPlan::Scene { layers })
    }
}

fn from_import(error: ImportError) -> PlayError {
    match error {
        ImportError::UnsupportedMedia { kind } => PlayError::Unsupported { kind },
        ImportError::MissingMedia { path } => PlayError::MissingFile { path },
        ImportError::PathEscapes { directory, file } => PlayError::PathEscapes { directory, file },
        ImportError::Io { path, source } => PlayError::Io { path, source },
        other => PlayError::Resolve(other),
    }
}

/// `file://` URL for the resolved path, without canonicalizing it.
///
/// A relative path is prefixed with the current directory. Symlinks and `..`
/// components stay as [`resolve_media`](wallpaper_import::resolve_media)
/// returned them, so the URL names that file. Bytes other than `/` and
/// unreserved URL characters are percent-encoded, which keeps spaces, `?`,
/// and `#` inside the path.
fn file_url(path: &Path) -> Result<String, PlayError> {
    let owned;
    let absolute = if path.is_absolute() {
        path
    } else {
        owned = std::path::absolute(path).map_err(|source| PlayError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        owned.as_path()
    };

    let bytes = std::os::unix::ffi::OsStrExt::as_bytes(absolute.as_os_str());
    let mut url = String::from("file://");
    for &byte in bytes {
        if is_file_url_byte(byte) {
            url.push(byte as char);
        } else {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            url.push('%');
            url.push(HEX[(byte >> 4) as usize] as char);
            url.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    Ok(url)
}

/// Unreserved characters plus `/`, which separates path segments.
fn is_file_url_byte(byte: u8) -> bool {
    matches!(
        byte,
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'.' | b'_' | b'~'
    )
}

/// `mpv` flag that repeats the current file forever.
///
/// `--loop-file=inf` loops that file. It does not restart a playlist.
const MPV_INFINITE_FILE_LOOP: &str = "--loop-file=inf";

/// Executables [`launch`] uses for each [`PlayPlan`] variant.
///
/// The default video action is a muted Plasma wallpaper. `video` is the
/// program used only when [`Self::plasma`] is false: an explicit override
/// such as `--video-player`. That override gets `mpv` arguments. The default
/// web action is a muted Plasma wallpaper as well. `web` is the program used
/// only when [`Self::web_plasma`] is false.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Players {
    /// Video player used when [`Self::plasma`] is false. Default: `mpv`.
    pub video: PathBuf,
    /// Program that opens a web wallpaper when [`Self::web_plasma`] is false.
    /// Default: `xdg-open`.
    pub web: PathBuf,
    /// When true, a video plan is installed and selected as a Plasma wallpaper.
    ///
    /// The default is true. An explicit video player sets this to false and
    /// spawns [`Self::video`] instead. Clearing this flag does not change
    /// [`Self::web_plasma`].
    pub plasma: bool,
    /// When true, a web plan is installed and selected as a Plasma wallpaper.
    ///
    /// The default is true. An explicit web player sets this to false and
    /// spawns [`Self::web`] instead. [`Self::plasma`] does not change this
    /// flag.
    pub web_plasma: bool,
    /// `qdbus6` or `qdbus`, used to call [`PLASMA_DBUS_METHOD`].
    pub plasmashell: PathBuf,
    /// `$XDG_DATA_HOME` or `~/.local/share`. The wallpaper package is copied
    /// to `plasma/wallpapers/` inside this directory.
    pub plasma_data_home: PathBuf,
    /// Mute the Plasma video or web wallpaper. The default is true.
    ///
    /// Set this to false to pass sound on. An explicit `mpv` or web player
    /// override does not read this flag.
    pub muted: bool,
}

impl Default for Players {
    fn default() -> Self {
        Self {
            video: PathBuf::from("mpv"),
            web: PathBuf::from("xdg-open"),
            plasma: true,
            web_plasma: true,
            plasmashell: plasma::default_plasmashell_tool(),
            plasma_data_home: plasma::default_plasma_data_home(),
            muted: true,
        }
    }
}

/// Failure while starting the process for a [`PlayPlan`].
#[derive(Debug)]
pub enum LaunchError {
    /// `program` was not found.
    MissingPlayer { program: PathBuf, source: io::Error },
    /// Spawning `program` failed for a reason other than a missing executable.
    Spawn { program: PathBuf, source: io::Error },
    /// The Plasma shell scripting tool (`qdbus6` or `qdbus`) was not found.
    ///
    /// No video player and no web browser are started.
    MissingPlasmashell { program: PathBuf, source: io::Error },
    /// The user-local wallpaper package could not be written.
    PlasmaInstall { path: PathBuf, source: io::Error },
    /// The Plasma shell script could not be built for this file.
    PlasmaScript { message: String },
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaunchError::MissingPlayer { program, source } => {
                write!(
                    f,
                    "player executable not found: {}: {source}",
                    program.display()
                )
            }
            LaunchError::Spawn { program, source } => {
                write!(f, "failed to start {}: {source}", program.display())
            }
            LaunchError::MissingPlasmashell { program, source } => {
                write!(
                    f,
                    "plasmashell tool not found: {}: {source}",
                    program.display()
                )
            }
            LaunchError::PlasmaInstall { path, source } => {
                write!(
                    f,
                    "failed to install the Plasma wallpaper plugin into {}: {source}",
                    path.display()
                )
            }
            LaunchError::PlasmaScript { message } => {
                write!(f, "failed to build the Plasma wallpaper script: {message}")
            }
        }
    }
}

impl std::error::Error for LaunchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LaunchError::MissingPlayer { source, .. }
            | LaunchError::Spawn { source, .. }
            | LaunchError::MissingPlasmashell { source, .. }
            | LaunchError::PlasmaInstall { source, .. } => Some(source),
            LaunchError::PlasmaScript { .. } => None,
        }
    }
}

/// Start `plan`.
///
/// A video plan with [`Players::plasma`] set (the default) installs the
/// user-local video wallpaper plugin and runs the plasmashell tool with a
/// script that selects it. That path does not spawn [`Players::video`]. When
/// `plasma` is clear, a video plan spawns `players.video`. When `loops` is
/// set, the arguments are `--loop-file=inf` and then the resolved media path,
/// which is `mpv`'s infinite loop of that file. When `loops` is clear, the
/// only argument is the media path.
///
/// A web plan with [`Players::web_plasma`] set (the default) installs the
/// user-local web wallpaper plugin and runs the same plasmashell tool. That
/// path does not spawn [`Players::web`]. When `web_plasma` is clear, a web
/// plan spawns `players.web` with the file URL as its only argument.
///
/// A scene plan always installs the scene wallpaper plugin and runs the
/// plasmashell tool. It does not spawn [`Players::video`] or [`Players::web`],
/// even when an explicit video or web player cleared the Plasma flags.
///
/// The returned [`Child`] is still running. A missing executable is
/// [`LaunchError::MissingPlayer`], or [`LaunchError::MissingPlasmashell`]
/// for a Plasma path. A missing plasmashell tool does not fall back to
/// `mpv` or `xdg-open`.
///
/// A scene plan always uses `linux.wallpaper.scene`. The installed live
/// plugin is selected by [`play_entry`], not by this function.
pub fn launch(plan: &PlayPlan, players: &Players) -> Result<Child, LaunchError> {
    match plan {
        PlayPlan::Scene { layers } => plasma::launch_plasma_scene_wallpaper(layers, players),
        PlayPlan::Video { file, .. } if players.plasma => {
            plasma::launch_plasma_wallpaper(file, players)
        }
        PlayPlan::Web { url } if players.web_plasma => {
            plasma::launch_plasma_web_wallpaper(url, players)
        }
        PlayPlan::Video { file, loops } => {
            let mut command = Command::new(&players.video);
            if *loops {
                command.arg(MPV_INFINITE_FILE_LOOP);
            }
            command.arg(file);
            spawn_player(&players.video, command)
        }
        PlayPlan::Web { url } => {
            let mut command = Command::new(&players.web);
            command.arg(url);
            spawn_player(&players.web, command)
        }
    }
}

fn spawn_player(program: &Path, mut command: Command) -> Result<Child, LaunchError> {
    command.spawn().map_err(|source| {
        let program = program.to_path_buf();
        if source.kind() == io::ErrorKind::NotFound {
            LaunchError::MissingPlayer { program, source }
        } else {
            LaunchError::Spawn { program, source }
        }
    })
}

/// Failure while choosing and starting the first playable wallpaper.
#[derive(Debug)]
pub enum PlayFirstError {
    /// [`scan_library`](wallpaper_import::scan_library) failed.
    Scan(ImportError),
    /// The library has no video or web wallpaper.
    ///
    /// Scene, application, and other unsupported entries were skipped. No
    /// player process was started.
    NothingPlayable,
    /// The first video or web entry could not be turned into a [`PlayPlan`].
    Play(PlayError),
    /// The player for that plan could not be started.
    Launch(LaunchError),
}

impl fmt::Display for PlayFirstError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlayFirstError::Scan(source) => write!(f, "{source}"),
            PlayFirstError::NothingPlayable => {
                write!(f, "library has no video or web wallpaper")
            }
            PlayFirstError::Play(source) => write!(f, "{source}"),
            PlayFirstError::Launch(source) => write!(f, "{source}"),
        }
    }
}

impl std::error::Error for PlayFirstError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PlayFirstError::Scan(source) => Some(source),
            PlayFirstError::Play(source) => Some(source),
            PlayFirstError::Launch(source) => Some(source),
            PlayFirstError::NothingPlayable => None,
        }
    }
}

/// A wallpaper [`play_entry`] started.
pub struct Played {
    /// The plasmashell tool, or the explicit video or web player.
    pub child: Child,
    /// The static scene plugin is showing image and video layers only.
    ///
    /// The command prints [`PARTIAL_SCENE_NOTICE`] in this case. A live
    /// plugin selection leaves this false.
    pub partial_scene: bool,
}

/// Failure from [`play_entry`].
#[derive(Debug)]
pub enum PlayEntryError {
    /// [`plan_playback`] failed. Nothing was spawned.
    Play(PlayError),
    /// The player or plasmashell tool could not be started.
    Launch(LaunchError),
}

impl fmt::Display for PlayEntryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlayEntryError::Play(source) => write!(f, "{source}"),
            PlayEntryError::Launch(source) => write!(f, "{source}"),
        }
    }
}

impl std::error::Error for PlayEntryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PlayEntryError::Play(source) => Some(source),
            PlayEntryError::Launch(source) => Some(source),
        }
    }
}

/// Start one library entry.
///
/// A scene whose live Plasma plugin is installed is selected through that
/// plugin. The workshop project directory is written into the plasmashell
/// script. `linux.wallpaper.scene` is not installed for that play. This
/// includes a project that is only `scene.pkg` and has no image layers.
///
/// When that plugin is absent, this is [`plan_playback`] followed by
/// [`launch`]. A scene with an image or video layer sets
/// [`Played::partial_scene`]. A scene with nothing to show returns
/// [`PlayError::Unsupported`] and spawns nothing.
///
/// Video and web entries are unchanged. [`Players::plasma`] and
/// [`Players::web_plasma`] do not change scene playback.
pub fn play_entry(entry: &LibraryEntry, players: &Players) -> Result<Played, PlayEntryError> {
    if is_scene_entry(entry) && plasma::live_scene_plugin_installed() {
        let child =
            plasma::launch_live_scene(&entry.directory, players).map_err(PlayEntryError::Launch)?;
        return Ok(Played {
            child,
            partial_scene: false,
        });
    }
    let plan = plan_playback(entry).map_err(PlayEntryError::Play)?;
    let partial_scene = matches!(plan, PlayPlan::Scene { .. });
    let child = launch(&plan, players).map_err(PlayEntryError::Launch)?;
    Ok(Played {
        child,
        partial_scene,
    })
}

fn is_scene_entry(entry: &LibraryEntry) -> bool {
    matches!(&entry.wallpaper, Wallpaper::Unsupported { kind } if is_scene(kind))
}

/// Scan `library` and start the first video or web wallpaper.
///
/// Entries come from [`scan_library`](wallpaper_import::scan_library), in
/// directory path order. Scene, application, and other unsupported entries
/// are skipped. A scene is skipped here even when its image layers could be
/// played by [`plan_playback`]. Play that item by id. The first video entry
/// is started with [`launch`]: a muted Plasma wallpaper of the resolved media
/// file, unless `players.plasma` is clear. The first web entry, when no video
/// sorts earlier, is a muted
/// Plasma wallpaper of the file URL, unless `players.web_plasma` is clear.
/// Later entries are left alone.
///
/// A library whose entries are all unsupported, including a library of only
/// scene projects, returns [`PlayFirstError::NothingPlayable`] and starts no
/// process. A video or web entry whose media cannot be resolved returns that
/// error and does not try a later project.
///
/// The returned [`Child`] is still running. `players` chooses the executables
/// the same way [`launch`] does.
pub fn play_first(library: impl AsRef<Path>, players: &Players) -> Result<Child, PlayFirstError> {
    let entries = scan_library(library).map_err(PlayFirstError::Scan)?;
    for entry in &entries {
        match &entry.wallpaper {
            Wallpaper::Unsupported { .. } => continue,
            Wallpaper::Video { .. } | Wallpaper::Web { .. } => {
                let plan = plan_playback(entry).map_err(PlayFirstError::Play)?;
                return launch(&plan, players).map_err(PlayFirstError::Launch);
            }
        }
    }
    Err(PlayFirstError::NothingPlayable)
}
