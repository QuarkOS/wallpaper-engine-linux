//! Read Wallpaper Engine project directories the user already owns.
//!
//! Wallpaper Engine describes each project with a `project.json` file. The
//! public fields this crate uses are `type`, `file`, and `title`. Video and
//! web projects become playable variants. Scene, application, and any other
//! type become [`Wallpaper::Unsupported`].
//!
//! [`load_wallpaper`] reads one project directory. [`scan_library`] walks the
//! immediate children of a library root, such as a Steam workshop folder, and
//! returns each project that loads. [`resolve_media`] joins a video or web
//! entry's project directory with its relative `file` and returns that path
//! when the file is on disk inside the project.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

/// A wallpaper loaded from a project directory.
///
/// `file` is the project-relative path stored in `project.json`. `title` is
/// the project title. Unsupported projects keep the original `type` string as
/// `kind` so callers can tell scene, application, and unknown types apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wallpaper {
    Video { file: String, title: String },
    Web { file: String, title: String },
    Unsupported { kind: String },
}

/// A project directory found under a library root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryEntry {
    /// Directory that contains this project's `project.json`.
    pub directory: PathBuf,
    /// Wallpaper loaded from that directory.
    pub wallpaper: Wallpaper,
}

/// Failure while reading a project directory.
#[derive(Debug)]
pub enum ImportError {
    /// `path` exists but is not a directory.
    NotADirectory(PathBuf),
    /// The directory has no `project.json`.
    MissingProjectJson(PathBuf),
    /// Reading `project.json` failed.
    Io { path: PathBuf, source: io::Error },
    /// `project.json` is not a JSON object this reader understands.
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    /// A video or web project is missing `file` or `title`, or `type` is absent.
    MissingField { field: &'static str },
    /// Scene, application, and other non-playable types have no media file.
    UnsupportedMedia { kind: String },
    /// The project-relative media file is not a file on disk.
    MissingMedia { path: PathBuf },
    /// `file` is absolute or climbs out of the project directory.
    PathEscapes { directory: PathBuf, file: String },
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::NotADirectory(path) => {
                write!(
                    f,
                    "wallpaper project is not a directory: {}",
                    path.display()
                )
            }
            ImportError::MissingProjectJson(path) => {
                write!(f, "missing project.json at {}", path.display())
            }
            ImportError::Io { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
            ImportError::Json { path, source } => {
                write!(f, "invalid project.json at {}: {source}", path.display())
            }
            ImportError::MissingField { field } => {
                write!(f, "project.json is missing required field `{field}`")
            }
            ImportError::UnsupportedMedia { kind } => {
                write!(f, "wallpaper type `{kind}` has no media file")
            }
            ImportError::MissingMedia { path } => {
                write!(f, "media file not found: {}", path.display())
            }
            ImportError::PathEscapes { directory, file } => {
                write!(
                    f,
                    "media path `{file}` escapes project directory {}",
                    directory.display()
                )
            }
        }
    }
}

impl std::error::Error for ImportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ImportError::Io { source, .. } => Some(source),
            ImportError::Json { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Fields read from the public `project.json` shape. Other keys are ignored.
#[derive(Debug, Deserialize)]
struct ProjectFile {
    #[serde(rename = "type")]
    kind: Option<String>,
    file: Option<String>,
    title: Option<String>,
}

/// Load `dir/project.json` into a [`Wallpaper`].
///
/// `dir` must be a directory. The media file named by `file` is not opened;
/// this function only classifies the project description.
pub fn load_wallpaper(dir: impl AsRef<Path>) -> Result<Wallpaper, ImportError> {
    let dir = dir.as_ref();
    if !dir.is_dir() {
        return Err(ImportError::NotADirectory(dir.to_path_buf()));
    }

    let path = dir.join("project.json");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(ImportError::MissingProjectJson(path));
        }
        Err(source) => return Err(ImportError::Io { path, source }),
    };

    let project: ProjectFile =
        serde_json::from_str(&text).map_err(|source| ImportError::Json { path, source })?;

    classify(project)
}

/// Scan the immediate children of a library root.
///
/// A Steam workshop folder is a parent of many project directories. Each child
/// directory that [`load_wallpaper`] accepts becomes a [`LibraryEntry`].
/// Children that are not directories, and children that fail to load (no
/// `project.json`, invalid JSON, or a missing required field), are skipped so
/// one junk child does not fail the scan. Entries are sorted by directory path.
///
/// `root` must be a directory. An empty directory returns an empty list.
pub fn scan_library(root: impl AsRef<Path>) -> Result<Vec<LibraryEntry>, ImportError> {
    let root = root.as_ref();
    if !root.is_dir() {
        return Err(ImportError::NotADirectory(root.to_path_buf()));
    }

    let mut entries = Vec::new();
    let children = fs::read_dir(root).map_err(|source| ImportError::Io {
        path: root.to_path_buf(),
        source,
    })?;

    for child in children {
        let child = child.map_err(|source| ImportError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let path = child.path();
        if !path.is_dir() {
            continue;
        }
        match load_wallpaper(&path) {
            Ok(wallpaper) => entries.push(LibraryEntry {
                directory: path,
                wallpaper,
            }),
            Err(_) => continue,
        }
    }

    entries.sort_by(|left, right| left.directory.cmp(&right.directory));
    Ok(entries)
}

fn classify(project: ProjectFile) -> Result<Wallpaper, ImportError> {
    let kind = required_text(project.kind, "type")?;
    match kind.to_ascii_lowercase().as_str() {
        "video" => Ok(Wallpaper::Video {
            file: required_text(project.file, "file")?,
            title: required_text(project.title, "title")?,
        }),
        "web" => Ok(Wallpaper::Web {
            file: required_text(project.file, "file")?,
            title: required_text(project.title, "title")?,
        }),
        _ => Ok(Wallpaper::Unsupported { kind }),
    }
}

fn required_text(value: Option<String>, field: &'static str) -> Result<String, ImportError> {
    match value {
        Some(value) if !value.trim().is_empty() => Ok(value.trim().to_string()),
        _ => Err(ImportError::MissingField { field }),
    }
}

/// Resolve the on-disk media file for a video or web [`LibraryEntry`].
///
/// The returned path is `entry.directory` joined with the project-relative
/// `file`. The file must exist. Unsupported wallpapers have no `file`, so this
/// returns [`ImportError::UnsupportedMedia`] without building a path. An
/// absolute `file`, a `file` that climbs out of the project directory with
/// `..`, or a symlink whose target leaves the project directory returns
/// [`ImportError::PathEscapes`].
pub fn resolve_media(entry: &LibraryEntry) -> Result<PathBuf, ImportError> {
    let file = match &entry.wallpaper {
        Wallpaper::Video { file, .. } | Wallpaper::Web { file, .. } => file,
        Wallpaper::Unsupported { kind } => {
            return Err(ImportError::UnsupportedMedia { kind: kind.clone() });
        }
    };

    if !relative_file_stays_inside(Path::new(file)) {
        return Err(ImportError::PathEscapes {
            directory: entry.directory.clone(),
            file: file.clone(),
        });
    }

    let path = entry.directory.join(file);
    if !path.is_file() {
        return Err(ImportError::MissingMedia { path });
    }

    if !canonical_file_stays_inside(&entry.directory, &path)? {
        return Err(ImportError::PathEscapes {
            directory: entry.directory.clone(),
            file: file.clone(),
        });
    }

    Ok(path)
}

/// `file` is a relative path that never climbs above the project directory.
///
/// Absolute paths are rejected because [`Path::join`] would discard the
/// project directory. A trailing climb such as `clips/../wallpaper.mp4` is
/// allowed when an earlier normal component keeps the result inside.
fn relative_file_stays_inside(file: &Path) -> bool {
    if file.is_absolute() {
        return false;
    }

    let mut depth = 0usize;
    for component in file.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }

    true
}

/// The opened file's canonical path is still inside the canonical project directory.
///
/// Lexical checks miss a symlink that points outside the project.
fn canonical_file_stays_inside(directory: &Path, file: &Path) -> Result<bool, ImportError> {
    let directory = directory.canonicalize().map_err(|source| ImportError::Io {
        path: directory.to_path_buf(),
        source,
    })?;
    let canonical = match file.canonicalize() {
        Ok(path) => path,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(ImportError::MissingMedia {
                path: file.to_path_buf(),
            });
        }
        Err(source) => {
            return Err(ImportError::Io {
                path: file.to_path_buf(),
                source,
            });
        }
    };

    if canonical == directory {
        return Err(ImportError::MissingMedia {
            path: file.to_path_buf(),
        });
    }

    Ok(canonical.starts_with(&directory))
}
