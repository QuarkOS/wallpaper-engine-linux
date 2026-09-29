//! Local page for the workshop library.
//!
//! [`serve`] binds to `127.0.0.1` on an ephemeral port, prints that URL, and
//! serves the page. `GET /api/library` lists each workshop item. `POST
//! /api/play` with `{"id":"..."}` starts an item through
//! [`play_entry`](crate::play_entry). The page offers video and web items.
//! A scene is offered when the live Plasma scene plugin is installed. A
//! scene with nothing to show, and no live plugin, is an error and does not
//! start a player.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use serde::Deserialize;
use serde::Serialize;
use wallpaper_import::{scan_library, ImportError, LibraryEntry, Wallpaper};

use crate::{play_entry, LaunchError, PlayEntryError, PlayError, Players};

const INDEX_HTML: &str = include_str!("../../../ui/index.html");
const APP_JS: &str = include_str!("../../../ui/app.js");

/// Ports a local dev server often uses. This page does not bind them.
const RESERVED_PORTS: [u16; 3] = [3000, 5173, 8080];

struct State {
    library: Option<PathBuf>,
    players: Players,
}

/// Bind `127.0.0.1`, print `http://127.0.0.1:{port}`, and serve until the process ends.
///
/// `library` is the workshop directory, or `None` when none was found. The
/// page still loads in that case. `players` chooses how a video or web item
/// starts. The default video and web actions are muted Plasma wallpapers.
/// An explicit web player skips Plasma and opens the file URL.
pub fn serve(library: Option<PathBuf>, players: Players) -> io::Result<()> {
    let listener = bind_loopback()?;
    let port = listener.local_addr()?.port();
    println!("http://127.0.0.1:{port}");
    io::stdout().flush()?;

    let state = Arc::new(State { library, players });
    for incoming in listener.incoming() {
        let stream = match incoming {
            Ok(stream) => stream,
            Err(_) => continue,
        };
        let state = Arc::clone(&state);
        thread::spawn(move || {
            let _ = handle(stream, &state);
        });
    }
    Ok(())
}

fn bind_loopback() -> io::Result<TcpListener> {
    let mut fallback = None;
    for _ in 0..16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        if !RESERVED_PORTS.contains(&port) {
            return Ok(listener);
        }
        fallback = Some(listener);
    }
    fallback.ok_or_else(|| io::Error::new(io::ErrorKind::AddrNotAvailable, "could not bind"))
}

fn handle(mut stream: TcpStream, state: &State) -> io::Result<()> {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(_) => return Ok(()),
    };
    let path = request.path.as_str();
    match (request.method.as_str(), path) {
        ("GET", "/" | "/index.html") => write_bytes(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            INDEX_HTML.as_bytes(),
        ),
        ("GET", "/ui/app.js") => write_bytes(
            &mut stream,
            200,
            "text/javascript; charset=utf-8",
            APP_JS.as_bytes(),
        ),
        ("GET", "/ui/styles.css") => write_bytes(
            &mut stream,
            200,
            "text/css; charset=utf-8",
            stylesheet().as_bytes(),
        ),
        ("GET", "/api/library") => match library_json(state.library.as_deref()) {
            Ok(body) => write_bytes(&mut stream, 200, "application/json", &body),
            Err(error) => write_bytes(
                &mut stream,
                500,
                "text/plain; charset=utf-8",
                error.to_string().as_bytes(),
            ),
        },
        ("POST", "/api/play") => play(&mut stream, state, &request.body),
        ("GET", "/api/play") | ("POST", "/api/library") => write_bytes(
            &mut stream,
            405,
            "text/plain; charset=utf-8",
            b"method not allowed",
        ),
        _ => write_bytes(&mut stream, 404, "text/plain; charset=utf-8", b"not found"),
    }
}

fn library_json(library: Option<&Path>) -> io::Result<Vec<u8>> {
    let live_scene = crate::live_scene_plugin_installed();
    let Some(library) = library else {
        return json(&LibraryBody {
            found: false,
            path: None,
            live_scene,
            items: Vec::new(),
        });
    };
    let entries = scan_library(library).map_err(io_error)?;
    let mut items: Vec<ItemBody> = entries.iter().map(item_body).collect();
    items.sort_by(|left, right| left.id.cmp(&right.id));
    json(&LibraryBody {
        found: true,
        path: Some(library.display().to_string()),
        live_scene,
        items,
    })
}

fn play(stream: &mut TcpStream, state: &State, body: &[u8]) -> io::Result<()> {
    let id = match serde_json::from_slice::<PlayRequest>(body) {
        Ok(request) if !request.id.is_empty() => request.id,
        _ => {
            let payload = PlayBody {
                ok: false,
                id: String::new(),
                error: Some("play request needs an id".to_string()),
            };
            return write_json(stream, 400, &payload);
        }
    };

    let Some(library) = state.library.as_deref() else {
        return write_json(
            stream,
            400,
            &PlayBody {
                ok: false,
                id: id.clone(),
                error: Some(format!("could not play {id}: no workshop library found")),
            },
        );
    };

    match start_item(library, &id, &state.players) {
        Ok(()) => write_json(
            stream,
            200,
            &PlayBody {
                ok: true,
                id,
                error: None,
            },
        ),
        Err(error) => write_json(
            stream,
            400,
            &PlayBody {
                ok: false,
                id: id.clone(),
                error: Some(format!("could not play {id}: {error}")),
            },
        ),
    }
}

fn start_item(library: &Path, id: &str, players: &Players) -> Result<(), PlayFailure> {
    let entries = scan_library(library).map_err(PlayFailure::Scan)?;
    let entry = entries
        .iter()
        .find(|entry| entry_id(entry) == id)
        .ok_or_else(|| PlayFailure::UnknownId(id.to_string()))?;
    let played = play_entry(entry, players).map_err(PlayFailure::from)?;
    let mut child = played.child;
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn item_body(entry: &LibraryEntry) -> ItemBody {
    ItemBody {
        id: entry_id(entry),
        kind: entry_type(&entry.wallpaper),
        title: entry_title(entry),
    }
}

fn entry_id(entry: &LibraryEntry) -> String {
    entry
        .directory
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn entry_type(wallpaper: &Wallpaper) -> String {
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

/// Share directory an RPM uses for `ui/styles.css`.
///
/// `WALLPAPER_DATA_DIR` replaces this directory so a test can point at a
/// temp root. An empty value keeps the default.
const DEFAULT_DATA_DIR: &str = "/usr/share/wallpaper-engine-linux";

fn data_dir_from(override_dir: Option<&OsStr>) -> PathBuf {
    match override_dir {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => PathBuf::from(DEFAULT_DATA_DIR),
    }
}

fn installed_data_dir() -> PathBuf {
    data_dir_from(env::var_os("WALLPAPER_DATA_DIR").as_deref())
}

/// CSS for the library page.
///
/// Search order: the copy next to the crate sources, `ui/styles.css` in the
/// working directory, `ui/styles.css` beside the executable, then
/// `ui/styles.css` in the installed data directory
/// (`/usr/share/wallpaper-engine-linux`, or `WALLPAPER_DATA_DIR`).
pub fn library_stylesheet() -> String {
    let exe_dir = env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    read_stylesheet(&stylesheet_candidates(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ui/styles.css"),
        PathBuf::from("ui/styles.css"),
        exe_dir,
        &installed_data_dir(),
    ))
}

fn stylesheet_candidates(
    manifest_css: PathBuf,
    cwd_css: PathBuf,
    exe_dir: Option<PathBuf>,
    data_dir: &Path,
) -> Vec<PathBuf> {
    let mut paths = vec![manifest_css, cwd_css];
    if let Some(dir) = exe_dir {
        paths.push(dir.join("ui/styles.css"));
    }
    paths.push(data_dir.join("ui/styles.css"));
    paths
}

fn read_stylesheet(paths: &[PathBuf]) -> String {
    for path in paths {
        if let Ok(text) = fs::read_to_string(path) {
            return text;
        }
    }
    String::new()
}

fn stylesheet() -> String {
    library_stylesheet()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{data_dir_from, read_stylesheet, stylesheet_candidates};

    #[test]
    fn stylesheet_lookup_finds_usr_share_file_when_other_candidates_missing() {
        let root = std::env::temp_dir().join(format!(
            "wallpaper-stylesheet-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        let data_dir = root.join("usr/share/wallpaper-engine-linux");
        let installed = data_dir.join("ui/styles.css");
        fs::create_dir_all(installed.parent().expect("ui parent")).expect("mkdir");
        let marker = "/* share stylesheet */\n";
        fs::write(&installed, marker).expect("write css");

        let manifest = root.join("crate/ui/styles.css");
        let cwd = root.join("cwd/ui/styles.css");
        let exe_dir = root.join("exe");
        assert!(!manifest.exists());
        assert!(!cwd.exists());
        assert!(!exe_dir.join("ui/styles.css").exists());

        let paths = stylesheet_candidates(manifest, cwd, Some(exe_dir), &data_dir);
        assert_eq!(
            paths.last().map(PathBuf::as_path),
            Some(installed.as_path()),
            "the last candidate is the /usr/share stylesheet"
        );
        assert_eq!(read_stylesheet(&paths), marker);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn data_dir_override_replaces_the_default_share_directory() {
        assert_eq!(
            data_dir_from(None),
            PathBuf::from("/usr/share/wallpaper-engine-linux")
        );
        assert_eq!(
            data_dir_from(Some(OsStr::new(""))),
            PathBuf::from("/usr/share/wallpaper-engine-linux")
        );
        let custom = Path::new("/tmp/wallpaper-data-root");
        assert_eq!(data_dir_from(Some(custom.as_os_str())), custom);
    }
}

#[derive(Serialize)]
struct LibraryBody {
    found: bool,
    path: Option<String>,
    #[serde(rename = "liveScene")]
    live_scene: bool,
    items: Vec<ItemBody>,
}

#[derive(Serialize)]
struct ItemBody {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    title: String,
}

#[derive(Deserialize)]
struct PlayRequest {
    id: String,
}

#[derive(Serialize)]
struct PlayBody {
    ok: bool,
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

enum PlayFailure {
    Scan(ImportError),
    UnknownId(String),
    Play(PlayError),
    Launch(LaunchError),
}

impl From<PlayEntryError> for PlayFailure {
    fn from(source: PlayEntryError) -> Self {
        match source {
            PlayEntryError::Play(source) => PlayFailure::Play(source),
            PlayEntryError::Launch(source) => PlayFailure::Launch(source),
        }
    }
}

impl std::fmt::Display for PlayFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlayFailure::Scan(source) => write!(formatter, "{source}"),
            PlayFailure::UnknownId(id) => write!(formatter, "unknown wallpaper id `{id}`"),
            PlayFailure::Play(source) => write!(formatter, "{source}"),
            PlayFailure::Launch(source) => write!(formatter, "{source}"),
        }
    }
}

fn json<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(io_error)
}

fn write_json(stream: &mut TcpStream, status: u16, value: &impl Serialize) -> io::Result<()> {
    write_bytes(stream, status, "application/json", &json(value)?)
}

fn write_bytes(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> io::Result<Request> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "client closed",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = find_slice(&buffer, b"\r\n\r\n") {
            break position + 4;
        }
        if buffer.len() > 64 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "headers too large",
            ));
        }
    };

    let header_text = String::from_utf8_lossy(&buffer[..header_end]);
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let raw_path = parts.next().unwrap_or("/");
    let path = raw_path.split('?').next().unwrap_or("/").to_string();

    let mut content_length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    if content_length > 64 * 1024 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "body too large"));
    }
    while buffer.len() < header_end + content_length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let end = header_end + content_length;
    let body = if buffer.len() >= end {
        buffer[header_end..end].to_vec()
    } else {
        buffer[header_end..].to_vec()
    };
    Ok(Request { method, path, body })
}

fn find_slice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn io_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::Other, error.to_string())
}
