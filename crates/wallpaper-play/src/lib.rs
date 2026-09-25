//! Turn a resolved video or web wallpaper into a play plan.
//!
//! A desktop player scans a library with [`wallpaper_import::scan_library`],
//! then calls [`plan_playback`] on each entry. Video plans name the resolved
//! file and loop it. Web plans are a `file://` URL to the resolved HTML file.
//! Scene, application, and other unsupported types, and video or web projects
//! whose media file is missing, are errors. This crate does not render
//! wallpapers.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use wallpaper_import::{resolve_media, ImportError, LibraryEntry, Wallpaper};

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
