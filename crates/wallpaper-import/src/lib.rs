//! Read Wallpaper Engine project directories the user already owns.
//!
//! Wallpaper Engine describes each project with a `project.json` file. The
//! public fields this crate uses are `type`, `file`, and `title`. Video and
//! web projects become playable variants. Scene, application, and any other
//! type become [`Wallpaper::Unsupported`].
//!
//! [`load_wallpaper`] reads one project directory. [`scan_library`] walks the
//! immediate children of a library root, such as a Steam workshop folder, and
//! returns each project that loads.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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
