//! Turn a resolved video or web wallpaper into a play plan and start it.
//!
//! A desktop player scans a library with [`wallpaper_import::scan_library`],
//! then calls [`plan_playback`] on each entry. [`launch`] starts that plan as
//! a process. A video plan runs `mpv` with an infinite loop of the resolved
//! file. A web plan runs `xdg-open` with the `file://` URL. Scene,
//! application, and other unsupported types, and video or web projects whose
//! media file is missing, are errors from [`plan_playback`]. [`play_first`]
//! scans a library, skips unsupported entries, and [`launch`]es the first
//! video or web wallpaper. This crate does not render wallpapers.
//!
//! The `wallpaper` command calls [`play_first`] on a workshop library.
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

pub mod steam_root;

/// What a desktop player should run for one library entry.
///
/// Video playback loops. Web playback loads the HTML file through a `file://`
/// URL. The player does not choose a different file than the one
/// [`resolve_media`](wallpaper_import::resolve_media) returned.
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
}

/// Failure while turning a library entry into a [`PlayPlan`].
#[derive(Debug)]
pub enum PlayError {
    /// Scene, application, and other types this player does not play.
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
/// The web plan is a `file://` URL for that HTML file. Unsupported entries and
/// missing files return an error instead of a plan.
pub fn plan_playback(entry: &LibraryEntry) -> Result<PlayPlan, PlayError> {
    let file = resolve_media(entry).map_err(from_import)?;
    match &entry.wallpaper {
        Wallpaper::Video { .. } => Ok(PlayPlan::Video { file, loops: true }),
        Wallpaper::Web { .. } => Ok(PlayPlan::Web {
            url: file_url(&file)?,
        }),
        Wallpaper::Unsupported { kind } => Err(PlayError::Unsupported { kind: kind.clone() }),
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
/// The defaults are `mpv` for video and `xdg-open` for web. Tests and callers
/// that want a different program replace these paths. Arguments stay the same:
/// an infinite file loop plus the media path, or the file URL alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Players {
    /// Video player. Default: `mpv`.
    pub video: PathBuf,
    /// Program that opens a web wallpaper. Default: `xdg-open`.
    pub web: PathBuf,
}

impl Default for Players {
    fn default() -> Self {
        Self {
            video: PathBuf::from("mpv"),
            web: PathBuf::from("xdg-open"),
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
        }
    }
}

impl std::error::Error for LaunchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LaunchError::MissingPlayer { source, .. } | LaunchError::Spawn { source, .. } => {
                Some(source)
            }
        }
    }
}

/// Start `plan` as a child process.
///
/// A video plan spawns `players.video`. When `loops` is set, the arguments are
/// `--loop-file=inf` and then the resolved media path, which is `mpv`'s
/// infinite loop of that file. When `loops` is clear, the only argument is the
/// media path. A web plan spawns `players.web` with the file URL as its only
/// argument.
///
/// The returned [`Child`] is still running. A missing executable is
/// [`LaunchError::MissingPlayer`].
pub fn launch(plan: &PlayPlan, players: &Players) -> Result<Child, LaunchError> {
    let program = match plan {
        PlayPlan::Video { .. } => &players.video,
        PlayPlan::Web { .. } => &players.web,
    };
    let mut command = Command::new(program);
    match plan {
        PlayPlan::Video { file, loops } => {
            if *loops {
                command.arg(MPV_INFINITE_FILE_LOOP);
            }
            command.arg(file);
        }
        PlayPlan::Web { url } => {
            command.arg(url);
        }
    }
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

/// Scan `library` and start the first video or web wallpaper.
///
/// Entries come from [`scan_library`](wallpaper_import::scan_library), in
/// directory path order. Scene, application, and other unsupported entries
/// are skipped. The first video entry is started with [`launch`]: an infinite
/// loop of the resolved media file. The first web entry, when no video sorts
/// earlier, is started with the file URL. Later entries are left alone.
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
