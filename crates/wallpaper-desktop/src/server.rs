//! Local page for the desktop window.
//!
//! The page is the same `ui/` files `wallpaper ui` serves, plus settings.
//! `GET /api/library` lists Steam and the extra folder. `liveScene` is true
//! when the Wallpaper Engine for KDE plugin is installed. `POST /api/play`
//! calls [`plan_playback`](wallpaper_play::plan_playback). A scene then uses
//! that plugin when it is installed, and the partial static scene view when
//! it is not. A scene with nothing to show, and no live plugin, is an error
//! and does not start a player. This function does not open a window.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::Deserialize;
use serde::Serialize;
use wallpaper_import::{scan_library, LibraryEntry, Wallpaper};
use wallpaper_play::{
    launch, launch_live_scene, live_scene_plugin_installed, live_scene_plugin_installed_in,
    plan_playback, LaunchError, PlayError, Players,
};

use crate::library::{collect_library, decode_id, encode_id, resolve_workshop, LibraryCard};
use crate::settings::{
    load_settings, set_autostart, set_extra_library, set_update_channel, Settings, UpdateChannel,
};
use crate::updates::{
    check_channel, download_failure, fetch_bytes, hash_failure, install_archive, sha256sums_match,
    CheckOutcome, UpdateOffer, ARCHIVE_NAME, GITHUB_RELEASES_URL,
};

const INDEX_HTML: &str = include_str!("../../../ui/index.html");
const APP_JS: &str = include_str!("../../../ui/app.js");

const RESERVED_PORTS: [u16; 3] = [3000, 5173, 8080];

struct State {
    steam_root: Option<PathBuf>,
    home: Option<PathBuf>,
    config_home: PathBuf,
    players: Players,
    /// Plasma data directories for the live scene plugin.
    ///
    /// `None` uses [`live_scene_plugin_installed`], which reads
    /// `WALLPAPER_PLASMA_DATA_DIRS`, `/usr/share`, `$XDG_DATA_HOME`, and
    /// `$XDG_DATA_DIRS`.
    plasma_data_dirs: Option<Vec<PathBuf>>,
    settings_lock: Mutex<()>,
    version: String,
    releases_url: Option<String>,
    install_dir: Option<PathBuf>,
    update_offer: Mutex<Option<UpdateOffer>>,
}

/// A loopback server. Dropping it stops the accept thread.
pub struct Server {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect((Ipv4Addr::LOCALHOST, self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Where the library page looks for Steam, settings, and players.
#[derive(Debug, Clone)]
pub struct DesktopOptions {
    pub steam_root: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub config_home: PathBuf,
    pub players: Players,
    /// Replaces the live-plugin search. `None` uses the wallpaper-play search,
    /// including `WALLPAPER_PLASMA_DATA_DIRS`.
    pub plasma_data_dirs: Option<Vec<PathBuf>>,
    /// Version compared with the selected release tag.
    pub version: String,
    /// GitHub releases URL, or a fixture. `None` uses the public repository.
    pub releases_url: Option<String>,
    /// Directory that receives unpacked binaries. `None` uses the executable directory.
    pub install_dir: Option<PathBuf>,
}

/// Bind `127.0.0.1` and serve until [`Server`] is dropped.
///
/// Does not open a window and does not require a display.
pub fn serve(options: DesktopOptions) -> io::Result<Server> {
    let listener = bind_loopback()?;
    let port = listener.local_addr()?.port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop);
    let state = Arc::new(State {
        steam_root: options.steam_root,
        home: options.home,
        config_home: options.config_home,
        players: options.players,
        plasma_data_dirs: options.plasma_data_dirs,
        settings_lock: Mutex::new(()),
        version: options.version,
        releases_url: options.releases_url,
        install_dir: options.install_dir,
        update_offer: Mutex::new(None),
    });
    let thread = thread::spawn(move || accept_loop(listener, stop_flag, state));
    Ok(Server {
        port,
        stop,
        thread: Some(thread),
    })
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

fn accept_loop(listener: TcpListener, stop: Arc<AtomicBool>, state: Arc<State>) {
    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(_) => continue,
        };
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let state = Arc::clone(&state);
        thread::spawn(move || {
            let _ = handle(stream, &state);
        });
    }
}

fn handle(mut stream: TcpStream, state: &State) -> io::Result<()> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(_) => return Ok(()),
    };
    dispatch(&mut stream, state, &request)
}

fn dispatch(stream: &mut TcpStream, state: &State, request: &Request) -> io::Result<()> {
    let path = request.path.as_str();
    match (request.method.as_str(), path) {
        ("GET", "/" | "/index.html") => write_bytes(
            stream,
            200,
            "text/html; charset=utf-8",
            INDEX_HTML.as_bytes(),
            None,
        ),
        ("GET", "/ui/app.js") => write_bytes(
            stream,
            200,
            "text/javascript; charset=utf-8",
            APP_JS.as_bytes(),
            None,
        ),
        ("GET", "/ui/styles.css") => {
            let css = stylesheet();
            write_bytes(stream, 200, "text/css; charset=utf-8", css.as_bytes(), None)
        }
        ("GET", "/api/library") => library(stream, state),
        ("GET", "/api/settings") => settings(stream, state),
        ("POST", "/api/library/extra") => add_extra(stream, state, &request.body),
        ("POST", "/api/autostart") => autostart(stream, state, &request.body),
        ("POST", "/api/updates/channel") => update_channel(stream, state, &request.body),
        ("POST", "/api/updates/check") => check_for_update(stream, state),
        ("POST", "/api/updates/download") => download_update(stream, state),
        ("POST", "/api/play") => play(stream, state, &request.body),
        ("GET", path) if path.starts_with("/api/preview/") => preview(
            stream,
            state,
            &path["/api/preview/".len()..],
            request.range.as_deref(),
        ),
        ("GET", "/api/play") | ("POST", "/api/library") | ("POST", "/api/settings") => write_bytes(
            stream,
            405,
            "text/plain; charset=utf-8",
            b"method not allowed",
            None,
        ),
        _ => write_bytes(stream, 404, "text/plain; charset=utf-8", b"not found", None),
    }
}

fn library(stream: &mut TcpStream, state: &State) -> io::Result<()> {
    let settings = read_settings(state);
    let steam = resolve_workshop(state.steam_root.as_deref(), state.home.as_deref());
    let extra = settings.extra_library.clone();
    let cards = collect_library(steam.as_deref(), extra.as_deref());
    let items = cards.iter().map(item_body).collect();
    let body = LibraryBody {
        found: steam.is_some(),
        path: steam.map(|path| path.display().to_string()),
        extra_library: extra.map(|path| path.display().to_string()),
        live_scene: scene_plugin_installed(state),
        items,
    };
    write_json(stream, 200, &body)
}

fn item_body(card: &LibraryCard) -> ItemBody {
    let preview = match (&card.preview_kind, &card.preview_path) {
        (Some(kind), Some(_)) => Some(PreviewBody {
            url: format!("/api/preview/{}", encode_id(&card.id)),
            kind: kind.clone(),
        }),
        _ => None,
    };
    ItemBody {
        id: card.id.clone(),
        kind: card.kind.clone(),
        title: card.title.clone(),
        preview,
    }
}

fn settings(stream: &mut TcpStream, state: &State) -> io::Result<()> {
    let settings = read_settings(state);
    write_json(stream, 200, &settings_body(&settings, &state.version))
}

fn settings_body(settings: &Settings, version: &str) -> SettingsBody {
    SettingsBody {
        extra_library: settings
            .extra_library
            .as_ref()
            .map(|path| path.display().to_string()),
        autostart_asked: settings.autostart_asked == Some(true),
        autostart: settings.autostart_enabled(),
        update_channel: settings.update_channel.as_str().to_string(),
        version: version.to_string(),
    }
}

fn add_extra(stream: &mut TcpStream, state: &State, body: &[u8]) -> io::Result<()> {
    let path = match serde_json::from_slice::<ExtraRequest>(body) {
        Ok(request) if !request.path.trim().is_empty() => PathBuf::from(request.path.trim()),
        _ => {
            return write_json(
                stream,
                400,
                &ExtraBody {
                    ok: false,
                    path: None,
                    error: Some("folder request needs a path".to_string()),
                },
            );
        }
    };
    let _guard = state
        .settings_lock
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    match set_extra_library(&state.config_home, &path) {
        Ok(settings) => {
            let stored = settings
                .extra_library
                .map(|path| path.display().to_string())
                .unwrap_or_default();
            write_json(
                stream,
                200,
                &ExtraBody {
                    ok: true,
                    path: Some(stored),
                    error: None,
                },
            )
        }
        Err(error) => write_json(
            stream,
            400,
            &ExtraBody {
                ok: false,
                path: None,
                error: Some(error.to_string()),
            },
        ),
    }
}

fn autostart(stream: &mut TcpStream, state: &State, body: &[u8]) -> io::Result<()> {
    let enabled = match serde_json::from_slice::<AutostartRequest>(body) {
        Ok(request) => request.enabled,
        Err(_) => {
            return write_json(
                stream,
                400,
                &AutostartBody {
                    ok: false,
                    autostart: false,
                    autostart_asked: false,
                    error: Some("autostart request needs enabled".to_string()),
                },
            );
        }
    };
    let _guard = state
        .settings_lock
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    match set_autostart(&state.config_home, enabled) {
        Ok(settings) => write_json(
            stream,
            200,
            &AutostartBody {
                ok: true,
                autostart: settings.autostart_enabled(),
                autostart_asked: settings.autostart_asked == Some(true),
                error: None,
            },
        ),
        Err(error) => write_json(
            stream,
            400,
            &AutostartBody {
                ok: false,
                autostart: false,
                autostart_asked: false,
                error: Some(error.to_string()),
            },
        ),
    }
}

fn update_channel(stream: &mut TcpStream, state: &State, body: &[u8]) -> io::Result<()> {
    let channel = match serde_json::from_slice::<ChannelRequest>(body) {
        Ok(request) => match UpdateChannel::parse(request.channel.trim()) {
            Some(channel) => channel,
            None => {
                return write_json(
                    stream,
                    400,
                    &ChannelBody {
                        ok: false,
                        update_channel: None,
                        error: Some(
                            "update channel must be release, preview, or nightly".to_string(),
                        ),
                    },
                );
            }
        },
        Err(_) => {
            return write_json(
                stream,
                400,
                &ChannelBody {
                    ok: false,
                    update_channel: None,
                    error: Some("update channel request needs channel".to_string()),
                },
            );
        }
    };
    let _guard = state
        .settings_lock
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    clear_offer(state);
    match set_update_channel(&state.config_home, channel) {
        Ok(settings) => write_json(
            stream,
            200,
            &ChannelBody {
                ok: true,
                update_channel: Some(settings.update_channel.as_str().to_string()),
                error: None,
            },
        ),
        Err(error) => write_json(
            stream,
            400,
            &ChannelBody {
                ok: false,
                update_channel: None,
                error: Some(error.to_string()),
            },
        ),
    }
}

fn check_for_update(stream: &mut TcpStream, state: &State) -> io::Result<()> {
    let channel = read_settings(state).update_channel;
    let url = state
        .releases_url
        .clone()
        .unwrap_or_else(|| GITHUB_RELEASES_URL.to_string());
    let outcome = match fetch_bytes(&url) {
        Ok(body) => check_channel(&body, channel, &state.version),
        Err(message) => CheckOutcome::Failed(message),
    };
    let mut offer = state
        .update_offer
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    *offer = match &outcome {
        CheckOutcome::Available(offer) => Some(offer.clone()),
        _ => None,
    };
    drop(offer);
    write_json(stream, 200, &check_body(channel, &state.version, &outcome))
}

fn download_update(stream: &mut TcpStream, state: &State) -> io::Result<()> {
    let channel = read_settings(state).update_channel;
    let offer = {
        let guard = state
            .update_offer
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        guard.clone()
    };
    let Some(offer) = offer.filter(|offer| offer.channel == channel) else {
        return write_json(
            stream,
            200,
            &DownloadBody {
                ok: false,
                state: "error",
                message: None,
                path: None,
                error: Some("Check for updates before downloading.".to_string()),
            },
        );
    };
    let archive = match fetch_bytes(&offer.archive_url) {
        Ok(bytes) => bytes,
        Err(_) => {
            return write_json(
                stream,
                200,
                &DownloadBody {
                    ok: false,
                    state: "error",
                    message: None,
                    path: None,
                    error: Some(download_failure()),
                },
            );
        }
    };
    let sums = match fetch_bytes(&offer.sums_url) {
        Ok(bytes) => bytes,
        Err(_) => {
            return write_json(
                stream,
                200,
                &DownloadBody {
                    ok: false,
                    state: "error",
                    message: None,
                    path: None,
                    error: Some(download_failure()),
                },
            );
        }
    };
    let sums = String::from_utf8_lossy(&sums);
    if !sha256sums_match(&sums, ARCHIVE_NAME, &archive) {
        return write_json(
            stream,
            200,
            &DownloadBody {
                ok: false,
                state: "error",
                message: None,
                path: None,
                error: Some(hash_failure()),
            },
        );
    }
    let install_dir = install_directory(state);
    let data_dir = state.config_home.join("wallpaper");
    match install_archive(&archive, &install_dir, &data_dir) {
        Ok(outcome) => write_json(
            stream,
            200,
            &DownloadBody {
                ok: true,
                state: outcome.state(),
                message: Some(outcome.message()),
                path: outcome.path().map(|path| path.display().to_string()),
                error: None,
            },
        ),
        Err(error) => write_json(
            stream,
            200,
            &DownloadBody {
                ok: false,
                state: "error",
                message: None,
                path: None,
                error: Some(error),
            },
        ),
    }
}

fn clear_offer(state: &State) {
    let mut offer = state
        .update_offer
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    *offer = None;
}

fn install_directory(state: &State) -> PathBuf {
    if let Some(dir) = &state.install_dir {
        return dir.clone();
    }
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn check_body(channel: UpdateChannel, current: &str, outcome: &CheckOutcome) -> UpdateBody {
    let (version, notes, error) = match outcome {
        CheckOutcome::Available(offer) => {
            (Some(offer.tag.clone()), Some(offer.notes.clone()), None)
        }
        CheckOutcome::Current { tag } => (Some(tag.clone()), None, None),
        CheckOutcome::None => (
            None,
            None,
            Some(format!("No release on the {} channel.", channel.as_str())),
        ),
        CheckOutcome::Failed(message) => (None, None, Some(message.clone())),
    };
    UpdateBody {
        ok: outcome.ok(),
        state: outcome.state(),
        channel: channel.as_str().to_string(),
        current: current.to_string(),
        version,
        notes,
        error,
    }
}

fn play(stream: &mut TcpStream, state: &State, body: &[u8]) -> io::Result<()> {
    let id = match serde_json::from_slice::<PlayRequest>(body) {
        Ok(request) if !request.id.is_empty() => request.id,
        _ => {
            return write_json(
                stream,
                400,
                &PlayBody {
                    ok: false,
                    id: String::new(),
                    error: Some("play request needs an id".to_string()),
                },
            );
        }
    };

    match start_item(state, &id) {
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

fn start_item(state: &State, id: &str) -> Result<(), PlayFailure> {
    let settings = read_settings(state);
    let steam = resolve_workshop(state.steam_root.as_deref(), state.home.as_deref());
    let mut roots = Vec::new();
    if let Some(steam) = steam {
        roots.push(steam);
    }
    if let Some(extra) = settings.extra_library {
        if extra.is_dir() && !roots.iter().any(|root| root == &extra) {
            roots.push(extra);
        }
    }
    if roots.is_empty() {
        return Err(PlayFailure::NoLibrary);
    }

    let mut found = None;
    for root in &roots {
        let entries = scan_library(root).map_err(PlayFailure::Scan)?;
        if let Some(entry) = entries.into_iter().find(|entry| entry_id(entry) == id) {
            found = Some(entry);
            break;
        }
    }
    let entry = found.ok_or_else(|| PlayFailure::UnknownId(id.to_string()))?;
    // `plan_playback` still runs for every play. A scene with the live plugin
    // installed is handed to that plugin, including a project that is only
    // scene.pkg. Otherwise the plan is launched: image and video layers use
    // the partial static plugin, and a scene with nothing to show errors.
    let plan = plan_playback(&entry);
    let mut child = if is_scene(&entry) && scene_plugin_installed(state) {
        launch_live_scene(&entry.directory, &state.players).map_err(PlayFailure::Launch)?
    } else {
        let plan = plan.map_err(PlayFailure::Play)?;
        launch(&plan, &state.players).map_err(PlayFailure::Launch)?
    };
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn is_scene(entry: &LibraryEntry) -> bool {
    matches!(
        &entry.wallpaper,
        Wallpaper::Unsupported { kind } if kind.eq_ignore_ascii_case("scene")
    )
}

/// Same package check as `wallpaper play`: `WALLPAPER_PLASMA_DATA_DIRS` when
/// set, otherwise the Plasma data directories.
fn scene_plugin_installed(state: &State) -> bool {
    match &state.plasma_data_dirs {
        Some(dirs) => live_scene_plugin_installed_in(dirs),
        None => live_scene_plugin_installed(),
    }
}

fn entry_id(entry: &LibraryEntry) -> String {
    entry
        .directory
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn preview(
    stream: &mut TcpStream,
    state: &State,
    raw_id: &str,
    range: Option<&str>,
) -> io::Result<()> {
    let Some(id) = decode_id(raw_id) else {
        return write_bytes(stream, 404, "text/plain; charset=utf-8", b"not found", None);
    };
    let settings = read_settings(state);
    let steam = resolve_workshop(state.steam_root.as_deref(), state.home.as_deref());
    let cards = collect_library(steam.as_deref(), settings.extra_library.as_deref());
    let Some(card) = cards.into_iter().find(|card| card.id == id) else {
        return write_bytes(stream, 404, "text/plain; charset=utf-8", b"not found", None);
    };
    let Some(path) = card.preview_path else {
        return write_bytes(stream, 404, "text/plain; charset=utf-8", b"not found", None);
    };
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => {
            return write_bytes(stream, 404, "text/plain; charset=utf-8", b"not found", None);
        }
    };
    let content_type = preview_content_type(&path);
    if let Some(range) = range {
        if let Some((start, end)) = parse_range(range, bytes.len() as u64) {
            let slice = &bytes[start as usize..=end as usize];
            let content_range = format!("bytes {start}-{end}/{}", bytes.len());
            return write_bytes(stream, 206, content_type, slice, Some(&content_range));
        }
        return write_bytes(
            stream,
            416,
            "text/plain; charset=utf-8",
            b"range not satisfiable",
            None,
        );
    }
    write_bytes(stream, 200, content_type, &bytes, None)
}

fn preview_content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("mp4") | Some("m4v") => "video/mp4",
        Some("webm") => "video/webm",
        Some("ogv") => "video/ogg",
        Some("mov") => "video/quicktime",
        Some("mkv") => "video/x-matroska",
        _ => "application/octet-stream",
    }
}

fn parse_range(header: &str, len: u64) -> Option<(u64, u64)> {
    if len == 0 {
        return None;
    }
    let value = header.trim().strip_prefix("bytes=")?;
    if value.contains(',') {
        return None;
    }
    let (start, end) = value.split_once('-')?;
    let start: u64 = if start.is_empty() {
        0
    } else {
        start.parse().ok()?
    };
    let end: u64 = if end.is_empty() {
        len - 1
    } else {
        end.parse().ok()?
    };
    if start > end || start >= len {
        return None;
    }
    Some((start, end.min(len - 1)))
}

fn read_settings(state: &State) -> Settings {
    load_settings(&state.config_home).unwrap_or_default()
}

fn stylesheet() -> String {
    wallpaper_play::ui::library_stylesheet()
}

#[derive(Serialize)]
struct LibraryBody {
    found: bool,
    path: Option<String>,
    #[serde(rename = "extraLibrary")]
    extra_library: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    preview: Option<PreviewBody>,
}

#[derive(Serialize)]
struct PreviewBody {
    url: String,
    kind: String,
}

#[derive(Serialize)]
struct SettingsBody {
    #[serde(rename = "extraLibrary")]
    extra_library: Option<String>,
    #[serde(rename = "autostartAsked")]
    autostart_asked: bool,
    autostart: bool,
    #[serde(rename = "updateChannel")]
    update_channel: String,
    version: String,
}

#[derive(Deserialize)]
struct ChannelRequest {
    channel: String,
}

#[derive(Serialize)]
struct ChannelBody {
    ok: bool,
    #[serde(rename = "updateChannel", skip_serializing_if = "Option::is_none")]
    update_channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct UpdateBody {
    ok: bool,
    state: &'static str,
    channel: String,
    current: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct DownloadBody {
    ok: bool,
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Deserialize)]
struct ExtraRequest {
    path: String,
}

#[derive(Serialize)]
struct ExtraBody {
    ok: bool,
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Deserialize)]
struct AutostartRequest {
    enabled: bool,
}

#[derive(Serialize)]
struct AutostartBody {
    ok: bool,
    autostart: bool,
    #[serde(rename = "autostartAsked")]
    autostart_asked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
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
    NoLibrary,
    Scan(wallpaper_import::ImportError),
    UnknownId(String),
    Play(PlayError),
    Launch(LaunchError),
}

impl std::fmt::Display for PlayFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlayFailure::NoLibrary => write!(formatter, "no workshop library found"),
            PlayFailure::Scan(source) => write!(formatter, "{source}"),
            PlayFailure::UnknownId(id) => write!(formatter, "unknown wallpaper id `{id}`"),
            PlayFailure::Play(source) => write!(formatter, "{source}"),
            PlayFailure::Launch(source) => write!(formatter, "{source}"),
        }
    }
}

fn write_json(stream: &mut TcpStream, status: u16, value: &impl Serialize) -> io::Result<()> {
    let body = serde_json::to_vec(value)
        .map_err(|error| io::Error::new(io::ErrorKind::Other, error.to_string()))?;
    write_bytes(stream, status, "application/json", &body, None)
}

fn write_bytes(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    content_range: Option<&str>,
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        206 => "Partial Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        416 => "Range Not Satisfiable",
        500 => "Internal Server Error",
        _ => "Error",
    };
    let range_header = content_range
        .map(|value| format!("Content-Range: {value}\r\n"))
        .unwrap_or_default();
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\n{range_header}Connection: close\r\nCache-Control: no-store\r\n\r\n",
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
    range: Option<String>,
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

    let mut headers = HashMap::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }
    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0usize);
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
    let range = headers.get("range").cloned();
    Ok(Request {
        method,
        path,
        body,
        range,
    })
}

fn find_slice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
