//! Workshop items from Steam plus one extra folder of projects.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use wallpaper_import::{scan_library, select_preview, workshop_dir, LibraryEntry, Wallpaper};
use wallpaper_play::steam_root::{find_steam_root, find_workshop_root};

/// One project shown on the library page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryCard {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub directory: PathBuf,
    /// `"image"` or `"video"` when [`select_preview`] found a file.
    pub preview_kind: Option<String>,
    pub preview_path: Option<PathBuf>,
}

/// Steam workshop directory for an explicit root, or the first one under `home`.
///
/// `steam_root` is used the same way as `wallpaper ui --steam-root`. Without
/// it, [`find_steam_root`] searches under `home`. A missing workshop is
/// `None`. The page still loads.
pub fn resolve_workshop(steam_root: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    let steam_root = if let Some(root) = steam_root {
        find_workshop_root(root).unwrap_or_else(|| root.to_path_buf())
    } else {
        find_steam_root(home?).ok()?
    };
    workshop_dir(steam_root).ok()
}

/// Projects from the Steam workshop, then from `extra` when that directory exists.
///
/// Each root is scanned in place. Nothing is copied. The same folder name in
/// both roots keeps the Steam project. Cards are sorted by id.
pub fn collect_library(steam: Option<&Path>, extra: Option<&Path>) -> Vec<LibraryCard> {
    let mut cards = Vec::new();
    let mut seen = HashSet::new();
    for root in [steam, extra].into_iter().flatten() {
        if !root.is_dir() {
            continue;
        }
        let Ok(entries) = scan_library(root) else {
            continue;
        };
        for entry in entries {
            let id = entry_id(&entry);
            if id.is_empty() || !seen.insert(id.clone()) {
                continue;
            }
            cards.push(card_from_entry(entry, id));
        }
    }
    cards.sort_by(|left, right| left.id.cmp(&right.id));
    cards
}

fn card_from_entry(entry: LibraryEntry, id: String) -> LibraryCard {
    let preview = select_preview(&entry.directory).ok().flatten();
    let (preview_kind, preview_path) = match preview {
        Some(preview) => (Some(preview.kind.as_str().to_string()), Some(preview.path)),
        None => (None, None),
    };
    LibraryCard {
        id,
        kind: entry_kind(&entry.wallpaper),
        title: entry_title(&entry),
        directory: entry.directory,
        preview_kind,
        preview_path,
    }
}

fn entry_id(entry: &LibraryEntry) -> String {
    entry
        .directory
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn entry_kind(wallpaper: &Wallpaper) -> String {
    match wallpaper {
        Wallpaper::Video { .. } => "video".to_string(),
        Wallpaper::Web { .. } => "web".to_string(),
        Wallpaper::Unsupported { kind } => kind.clone(),
    }
}

fn entry_title(entry: &LibraryEntry) -> String {
    match &entry.wallpaper {
        Wallpaper::Video { title, .. } | Wallpaper::Web { title, .. } => title.clone(),
        Wallpaper::Unsupported { .. } => title_from_project_json(&entry.directory),
    }
}

fn title_from_project_json(directory: &Path) -> String {
    let Ok(text) = fs::read_to_string(directory.join("project.json")) else {
        return String::new();
    };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|value| {
            value
                .get("title")
                .and_then(|title| title.as_str())
                .map(|title| title.trim().to_string())
        })
        .unwrap_or_default()
}

/// Percent-encode an id for a preview URL path segment.
pub fn encode_id(id: &str) -> String {
    let mut out = String::new();
    for byte in id.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Decode a preview URL segment. Rejects empty ids and any slash.
pub fn decode_id(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    let id = String::from_utf8(out).ok()?;
    if id.is_empty() || id.contains('/') || id.contains('\\') || id == "." || id == ".." {
        return None;
    }
    Some(id)
}
