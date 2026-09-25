//! Locate a Steam installation that contains a Wallpaper Engine workshop.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use wallpaper_import::workshop_dir;

/// Relative Steam roots on Linux, in the order they are searched.
///
/// `~/.steam/steam` is the usual symlink into the real install.
/// `~/.local/share/Steam` is the default data directory. `~/.steam/root` is
/// the other common link to that install. Extra libraries named in each
/// root's `steamapps/libraryfolders.vdf` are searched after that root.
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
                "no Steam workshop library under {} (tried .steam/steam, .local/share/Steam, and .steam/root, including libraries named in libraryfolders.vdf)",
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
/// `steamapps/workshop/content/431960`. When that directory is absent, libraries
/// named in `steamapps/libraryfolders.vdf` are checked in file order. A library
/// path that does not exist is skipped. A missing or unreadable vdf does not
/// drop the root itself. A missing root, or a root whose workshop path is
/// absent or not a directory, is skipped. When every candidate is skipped,
/// this returns [`SteamRootError::NotFound`].
pub fn find_steam_root(home: impl AsRef<Path>) -> Result<PathBuf, SteamRootError> {
    let home = home.as_ref();
    for relative in LINUX_STEAM_ROOTS {
        let root = home.join(relative);
        if let Some(found) = find_workshop_root(&root) {
            return Ok(found);
        }
    }
    Err(SteamRootError::NotFound {
        home: home.to_path_buf(),
    })
}

/// Workshop location for one Steam root.
///
/// The root itself is first. When it has no
/// `steamapps/workshop/content/431960`, each library named in
/// `steamapps/libraryfolders.vdf` is checked in file order. A quoted `path`
/// value is a library. A legacy numeric key whose quoted value is an absolute
/// path is one too. A library path that does not exist is skipped, as is a
/// library with no workshop directory. A missing or unreadable vdf leaves the
/// root as the only candidate and is not an error.
///
/// Returns the root, or the library directory, that contains the workshop.
/// `None` means neither the root nor a listed library has that directory.
pub fn find_workshop_root(root: impl AsRef<Path>) -> Option<PathBuf> {
    let root = root.as_ref();
    if workshop_dir(root).is_ok() {
        return Some(root.to_path_buf());
    }
    for library in libraries_named_in_vdf(root) {
        if !library.exists() {
            continue;
        }
        if workshop_dir(&library).is_ok() {
            return Some(library);
        }
    }
    None
}

/// Library directories quoted in `root/steamapps/libraryfolders.vdf`.
///
/// A missing file, a directory, or any other read error yields an empty list
/// so the caller can still use the root itself.
fn libraries_named_in_vdf(root: &Path) -> Vec<PathBuf> {
    let path = root.join("steamapps").join("libraryfolders.vdf");
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&bytes);
    library_paths(&text)
}

/// Quoted library paths from a `libraryfolders.vdf` body.
///
/// A modern entry is a `"path"` key and a quoted value. A legacy entry is a
/// numeric key whose quoted value looks like an absolute path. Other quoted
/// values, including app ids, are left alone. Paths are returned in file order.
fn library_paths(vdf: &str) -> Vec<PathBuf> {
    let vdf = vdf.strip_prefix('\u{feff}').unwrap_or(vdf);
    let mut paths = Vec::new();
    let mut chars = vdf.chars().peekable();
    let mut key: Option<String> = None;

    while let Some(next) = chars.peek().copied() {
        if next.is_whitespace() {
            chars.next();
            continue;
        }
        if next == '/' {
            chars.next();
            if chars.peek() == Some(&'/') {
                chars.next();
                for comment in chars.by_ref() {
                    if comment == '\n' {
                        break;
                    }
                }
            }
            continue;
        }
        if next == '"' {
            let quoted = read_quoted(&mut chars);
            match key.take() {
                None => key = Some(quoted),
                Some(name) => {
                    if let Some(path) = library_value(&name, &quoted) {
                        paths.push(PathBuf::from(path));
                    }
                }
            }
            continue;
        }
        if next == '{' || next == '}' {
            chars.next();
            key = None;
            continue;
        }
        chars.next();
    }
    paths
}

/// Library path named by this key/value pair, when the pair is a library entry.
fn library_value<'a>(name: &str, value: &'a str) -> Option<&'a str> {
    if value.is_empty() {
        return None;
    }
    if name.eq_ignore_ascii_case("path") {
        return Some(value);
    }
    if is_legacy_index(name) && looks_like_path(value) {
        return Some(value);
    }
    None
}

fn is_legacy_index(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit())
}

fn looks_like_path(value: &str) -> bool {
    if value.starts_with('/') || value.starts_with('~') {
        return true;
    }
    let mut chars = value.chars();
    match (chars.next(), chars.next()) {
        (Some(drive), Some(':')) if drive.is_ascii_alphabetic() => true,
        _ => false,
    }
}

fn read_quoted<I>(chars: &mut std::iter::Peekable<I>) -> String
where
    I: Iterator<Item = char>,
{
    assert_eq!(chars.next(), Some('"'));
    let mut out = String::new();
    while let Some(character) = chars.next() {
        match character {
            '"' => break,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some(other) => out.push(other),
                None => {}
            },
            other => out.push(other),
        }
    }
    out
}
