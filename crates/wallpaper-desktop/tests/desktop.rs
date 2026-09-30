use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use wallpaper_desktop::{
    autostart_desktop_path, collect_library, load_settings, pack_gzip_tar, serve, set_autostart,
    set_extra_library, set_update_channel, settings_path, sha256_hex, DesktopOptions, Server,
    UpdateChannel, ARCHIVE_NAME, NEXT_LAUNCH_MESSAGE, SUMS_NAME,
};
use wallpaper_play::{
    plasma_scene_wallpaper_dir, Players, LIVE_SCENE_PLUGIN_ID, PLASMA_SCENE_WALLPAPER_PLUGIN,
};

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

fn scratch_dir() -> PathBuf {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("wallpaper-desktop-{nanos}-{seq}"));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn workshop_path(steam_root: &Path) -> PathBuf {
    steam_root
        .join("steamapps")
        .join("workshop")
        .join("content")
        .join("431960")
}

fn write_project(dir: &Path, project_json: &str) {
    fs::create_dir_all(dir).expect("create project");
    fs::write(dir.join("project.json"), project_json).expect("write project.json");
}

fn write_file(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, body).expect("write file");
}

fn write_video(library: &Path, id: &str, file_name: &str) -> PathBuf {
    let project = library.join(id);
    write_project(
        &project,
        &format!(r#"{{"type":"video","file":"{file_name}","title":"Synthetic Video"}}"#),
    );
    let media = project.join(file_name);
    write_file(&media, "synthetic video bytes");
    media
}

fn write_video_preview(library: &Path, id: &str, preview: &str, kind_bytes: &str) -> PathBuf {
    let project = library.join(id);
    write_project(
        &project,
        &format!(
            r#"{{"type":"video","file":"wallpaper.mp4","title":"Synthetic Video","preview":"{preview}"}}"#
        ),
    );
    write_file(&project.join("wallpaper.mp4"), "synthetic video bytes");
    let preview_path = project.join(preview);
    write_file(&preview_path, kind_bytes);
    preview_path
}

fn write_scene(library: &Path, id: &str) {
    write_project(
        &library.join(id),
        r#"{"type":"scene","file":"scene.json","title":"Synthetic Scene","preview":"missing.png"}"#,
    );
}

fn write_argv_stub(dir: &Path, name: &str, record: &Path) -> PathBuf {
    let record = record.display().to_string();
    assert!(
        !record.contains('\''),
        "record path must be single-quote safe"
    );
    let stub = dir.join(name);
    let script = format!("#!/bin/sh\nprintf '%s\\0' \"$0\" \"$@\" > '{record}'\n");
    let mut file = File::create(&stub).expect("create stub");
    file.write_all(script.as_bytes()).expect("write stub");
    file.sync_all().expect("sync stub");
    let mut permissions = file.metadata().expect("stub metadata").permissions();
    permissions.set_mode(0o755);
    file.set_permissions(permissions).expect("chmod stub");
    drop(file);
    stub
}

fn read_argv(record: &Path) -> Vec<String> {
    let bytes = fs::read(record).expect("read argv record");
    bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8(part.to_vec()).expect("utf-8 argv"))
        .collect()
}

struct Stubs {
    video: PathBuf,
    web: PathBuf,
    video_record: PathBuf,
    web_record: PathBuf,
}

fn stubs_in(dir: &Path) -> Stubs {
    let video_record = dir.join("video-argv");
    let web_record = dir.join("web-argv");
    Stubs {
        video: write_argv_stub(dir, "video-stub", &video_record),
        web: write_argv_stub(dir, "web-stub", &web_record),
        video_record,
        web_record,
    }
}

fn players_for(stubs: &Stubs) -> Players {
    Players {
        video: stubs.video.clone(),
        web: stubs.web.clone(),
        plasma: false,
        web_plasma: false,
        ..Players::default()
    }
}

fn options(scratch: &Path, steam_root: PathBuf, players: Players) -> DesktopOptions {
    let install = scratch.join("install");
    fs::create_dir_all(&install).expect("install dir");
    DesktopOptions {
        steam_root: Some(steam_root),
        home: Some(scratch.join("home")),
        config_home: scratch.join("config"),
        players,
        plasma_data_dirs: Some(vec![scratch.join("no-live-plugin")]),
        version: "0.1.0".to_string(),
        releases_url: None,
        install_dir: Some(install),
    }
}

fn serve_options(options: DesktopOptions) -> Server {
    serve(options).expect("serve")
}

fn exchange(port: u16, request: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");
    stream.write_all(request.as_bytes()).expect("write request");
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(error) => panic!("read response: {error}"),
        }
    }
    let text = String::from_utf8(bytes).expect("utf-8 response");
    let (head, body) = text.split_once("\r\n\r\n").expect("http headers");
    let status = head
        .lines()
        .next()
        .expect("status line")
        .split_whitespace()
        .nth(1)
        .expect("status code")
        .parse()
        .expect("numeric status");
    (status, body.to_string())
}

fn get(port: u16, path: &str) -> (u16, String) {
    exchange(
        port,
        &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"),
    )
}

fn post_json(port: u16, path: &str, json: &str) -> (u16, String) {
    exchange(
        port,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{json}",
            json.len()
        ),
    )
}

fn files_named(dir: &Path, name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if !dir.exists() {
        return found;
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|file| file.to_str()) == Some(name) {
                found.push(path);
            }
        }
    }
    found
}

#[test]
fn extra_library_is_stored_and_the_next_scan_includes_it_without_copying() {
    let scratch = scratch_dir();
    let config = scratch.join("config");
    let extra = scratch.join("extra-library");
    let media = write_video(&extra, "2001", "wallpaper.mp4");
    let project_json = extra.join("2001/project.json");
    let before = fs::read(&project_json).expect("project bytes");

    let settings = set_extra_library(&config, &extra).expect("store extra library");
    assert_eq!(settings.extra_library.as_deref(), Some(extra.as_path()));

    let saved = fs::read_to_string(settings_path(&config)).expect("settings file");
    assert_eq!(
        saved,
        format!("{{\"extraLibrary\":\"{}\"}}\n", extra.display())
    );
    assert_eq!(files_named(&config, "project.json"), Vec::<PathBuf>::new());
    assert_eq!(
        fs::read(&project_json).expect("project still there"),
        before
    );
    assert!(media.is_file());

    let cards = collect_library(None, Some(extra.as_path()));
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].id, "2001");
    assert_eq!(cards[0].directory, extra.join("2001"));
    assert_eq!(cards[0].kind, "video");

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn autostart_yes_writes_the_desktop_file_and_no_records_the_decline() {
    let scratch = scratch_dir();
    let config = scratch.join("config");
    let desktop = autostart_desktop_path(&config);

    let fresh = load_settings(&config).expect("missing settings");
    assert!(fresh.needs_autostart_prompt());
    assert!(fresh.autostart_asked.is_none());
    assert!(!desktop.exists());

    let declined = set_autostart(&config, false).expect("decline");
    assert_eq!(declined.autostart_asked, Some(true));
    assert_eq!(declined.autostart, Some(false));
    assert!(!desktop.exists());
    assert_eq!(
        fs::read_to_string(settings_path(&config)).expect("decline settings"),
        "{\"autostartAsked\":true,\"autostart\":false}\n"
    );

    let enabled = set_autostart(&config, true).expect("enable");
    assert_eq!(enabled.autostart, Some(true));
    assert!(!enabled.needs_autostart_prompt());
    assert_eq!(
        fs::read_to_string(&desktop).expect("autostart file"),
        "[Desktop Entry]\nType=Application\nName=Wallpaper\nExec=wallpaper desktop\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
    );

    let disabled = set_autostart(&config, false).expect("disable");
    assert_eq!(disabled.autostart_asked, Some(true));
    assert_eq!(disabled.autostart, Some(false));
    assert!(!desktop.exists());
    assert_eq!(
        fs::read_to_string(settings_path(&config)).expect("still asked"),
        "{\"autostartAsked\":true,\"autostart\":false}\n"
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn adding_a_folder_keeps_the_autostart_choice() {
    let scratch = scratch_dir();
    let config = scratch.join("config");
    let extra = scratch.join("extra-library");
    fs::create_dir_all(&extra).expect("extra root");
    set_autostart(&config, false).expect("decline");
    set_extra_library(&config, &extra).expect("store extra");

    assert_eq!(
        fs::read_to_string(settings_path(&config)).expect("settings"),
        format!(
            "{{\"extraLibrary\":\"{}\",\"autostartAsked\":true,\"autostart\":false}}\n",
            extra.display()
        )
    );
    assert!(!autostart_desktop_path(&config).exists());

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn library_page_serves_previews_rescan_extra_folder_and_autostart() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("steam-root");
    let library = workshop_path(&steam_root);
    write_scene(&library, "1001");
    let image = write_video_preview(&library, "1003", "preview.png", "synthetic png bytes");
    write_video_preview(
        &library,
        "1004",
        "clips/preview.mp4",
        "synthetic preview video",
    );
    let stubs = stubs_in(&scratch);
    let server = serve_options(options(&scratch, steam_root.clone(), players_for(&stubs)));
    let port = server.port();
    assert_ne!(port, 3000);
    assert_ne!(port, 5173);
    assert_ne!(port, 8080);

    let (status, body) = get(port, "/api/settings");
    assert_eq!(status, 200, "{body}");
    let settings: serde_json::Value = serde_json::from_str(&body).expect("settings json");
    assert_eq!(settings["autostartAsked"], false);
    assert_eq!(settings["autostart"], false);
    assert!(settings["extraLibrary"].is_null());

    let (status, body) = get(port, "/api/library");
    assert_eq!(status, 200, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("library json");
    assert_eq!(payload["found"], true);
    assert_eq!(payload["liveScene"], false);
    assert_eq!(payload["path"], library.display().to_string());
    let items = payload["items"].as_array().expect("items");
    let image_item = items
        .iter()
        .find(|item| item["id"] == "1003")
        .expect("image preview item");
    assert_eq!(image_item["preview"]["kind"], "image");
    assert_eq!(image_item["preview"]["url"], "/api/preview/1003");
    let video_item = items
        .iter()
        .find(|item| item["id"] == "1004")
        .expect("video preview item");
    assert_eq!(video_item["preview"]["kind"], "video");
    assert_eq!(video_item["preview"]["url"], "/api/preview/1004");
    let scene = items
        .iter()
        .find(|item| item["id"] == "1001")
        .expect("scene");
    assert_eq!(scene["type"], "scene");
    assert!(scene.get("preview").is_none());

    let (status, body) = get(port, "/api/preview/1003");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, "synthetic png bytes");
    assert_eq!(
        fs::read(&image).expect("preview stays in the project"),
        body.as_bytes()
    );

    let (status, body) = get(port, "/api/preview/1004");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, "synthetic preview video");

    let (status, html) = get(port, "/");
    assert_eq!(status, 200, "{html}");
    assert!(html.contains("id=\"rescan\""));
    assert!(html.contains("id=\"extra-path\""));
    assert!(html.contains("id=\"autostart\""));
    assert!(html.contains("id=\"autostart-prompt\""));
    assert!(html.contains("hidden"));
    let (script_status, script) = get(port, "/ui/app.js");
    assert_eq!(script_status, 200, "{script}");
    assert!(script.contains("createElement(\"video\")"));
    assert!(script.contains("createElement(\"img\")"));
    assert!(script.contains("button.disabled = true"));
    assert!(script.contains("/api/play"));
    assert!(script.contains("No workshop items found."));

    let missing = scratch.join("missing-folder");
    let (status, body) = post_json(
        port,
        "/api/library/extra",
        &format!(r#"{{"path":"{}"}}"#, missing.display()),
    );
    assert!(status >= 400, "{body}");
    assert!(!settings_path(&server_config(&scratch)).exists());

    let extra = scratch.join("extra-library");
    let extra_media = write_video(&extra, "2001", "wallpaper.mp4");
    let (status, body) = post_json(
        port,
        "/api/library/extra",
        &format!(r#"{{"path":"{}"}}"#, extra.display()),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        fs::read_to_string(settings_path(&server_config(&scratch))).expect("settings"),
        format!("{{\"extraLibrary\":\"{}\"}}\n", extra.display())
    );
    assert_eq!(
        files_named(&server_config(&scratch), "project.json"),
        Vec::<PathBuf>::new()
    );

    let (status, body) = get(port, "/api/library");
    assert_eq!(status, 200, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("library after extra");
    assert_eq!(payload["extraLibrary"], extra.display().to_string());
    assert!(payload["items"]
        .as_array()
        .expect("items")
        .iter()
        .any(|item| item["id"] == "2001"));

    write_video(&library, "1009", "later.mp4");
    let (status, body) = get(port, "/api/library");
    assert_eq!(status, 200, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("rescan");
    let rescanned = payload["items"].as_array().expect("items");
    assert!(rescanned.iter().any(|item| item["id"] == "1009"));
    assert!(rescanned
        .iter()
        .find(|item| item["id"] == "1009")
        .expect("new video")
        .get("preview")
        .is_none());

    let (status, body) = post_json(port, "/api/play", r#"{"id":"1001"}"#);
    assert!(status >= 400, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("scene play");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["id"], "1001");
    thread::sleep(Duration::from_millis(150));
    assert!(!stubs.video_record.exists(), "scene play spawned a player");
    assert!(!stubs.web_record.exists(), "scene play spawned a player");

    let (status, body) = post_json(port, "/api/play", r#"{"id":"2001"}"#);
    assert_eq!(status, 200, "{body}");
    for _ in 0..100 {
        if stubs.video_record.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        read_argv(&stubs.video_record),
        vec![
            stubs.video.display().to_string(),
            "--loop-file=inf".to_string(),
            extra_media.display().to_string(),
        ]
    );

    let (status, body) = post_json(port, "/api/autostart", r#"{"enabled":false}"#);
    assert_eq!(status, 200, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("decline");
    assert_eq!(payload["autostartAsked"], true);
    assert_eq!(payload["autostart"], false);
    assert!(!autostart_desktop_path(&server_config(&scratch)).exists());
    assert_eq!(
        fs::read_to_string(settings_path(&server_config(&scratch))).expect("decline kept extra"),
        format!(
            "{{\"extraLibrary\":\"{}\",\"autostartAsked\":true,\"autostart\":false}}\n",
            extra.display()
        )
    );

    let (status, body) = post_json(port, "/api/autostart", r#"{"enabled":true}"#);
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        fs::read_to_string(autostart_desktop_path(&server_config(&scratch))).expect("desktop file"),
        "[Desktop Entry]\nType=Application\nName=Wallpaper\nExec=wallpaper desktop\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
    );

    let (status, body) = get(port, "/api/settings");
    assert_eq!(status, 200, "{body}");
    let settings: serde_json::Value = serde_json::from_str(&body).expect("settings after yes");
    assert_eq!(settings["autostartAsked"], true);
    assert_eq!(settings["autostart"], true);

    drop(server);
    let _ = fs::remove_dir_all(&scratch);
}

fn server_config(scratch: &Path) -> PathBuf {
    scratch.join("config")
}

fn desktop_bin() -> PathBuf {
    let current = std::env::current_exe().expect("test executable");
    let debug_dir = current
        .parent()
        .and_then(|dir| dir.parent())
        .expect("target debug dir");
    let bin = debug_dir.join("wallpaper-desktop");
    assert!(
        bin.is_file(),
        "wallpaper-desktop binary is missing at {}",
        bin.display()
    );
    bin
}

struct RunningDesktop {
    child: Child,
    port: u16,
}

impl Drop for RunningDesktop {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn desktop_without_a_display_serves_and_stays_up() {
    let scratch = scratch_dir();
    let config = scratch.join("config");
    let steam_root = scratch.join("steam-root");
    fs::create_dir_all(&steam_root).expect("steam root");
    let steam_root = steam_root.display().to_string();
    let mut child = Command::new(desktop_bin())
        .args(["--steam-root", &steam_root])
        .env("HOME", &scratch)
        .env("XDG_CONFIG_HOME", &config)
        .env_remove("WALLPAPER_VERSION")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wallpaper desktop");
    let stdout = child.stdout.take().expect("stdout");
    let (sender, receiver) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stdout);
        let mut line = String::new();
        let _ = std::io::BufRead::read_line(&mut reader, &mut line);
        let _ = sender.send(line);
    });
    let line = receiver
        .recv_timeout(Duration::from_secs(15))
        .expect("wallpaper desktop did not print a url");
    let url = line.trim();
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "expected a loopback url, got {url}"
    );
    let port: u16 = url
        .trim_start_matches("http://127.0.0.1:")
        .parse()
        .expect("port");
    let mut running = RunningDesktop { child, port };

    let (status, body) = get(running.port, "/api/settings");
    assert_eq!(status, 200, "{body}");
    let settings: serde_json::Value = serde_json::from_str(&body).expect("settings json");
    assert_eq!(settings["autostartAsked"], false);
    assert_eq!(settings["autostart"], false);
    assert_eq!(settings["updateChannel"], "release");
    assert_eq!(settings["version"], "0.2.0");
    match running.child.try_wait() {
        Ok(None) => {}
        Ok(Some(status)) => panic!("desktop exited without a display: {status}"),
        Err(error) => panic!("wait failed: {error}"),
    }

    let _ = fs::remove_dir_all(&scratch);
}

fn install_live_package(data_dir: &Path) {
    let package = data_dir
        .join("plasma")
        .join("wallpapers")
        .join(LIVE_SCENE_PLUGIN_ID);
    fs::create_dir_all(&package).expect("live package dir");
    fs::write(package.join("metadata.json"), "{}\n").expect("live metadata");
}

fn post_play(port: u16, id: &str) -> (u16, String) {
    let json = format!(r#"{{"id":"{id}"}}"#);
    let mut last = (0, String::new());
    for _ in 0..50 {
        last = post_json(port, "/api/play", &json);
        if last.0 == 200 || !last.1.contains("Text file busy") {
            return last;
        }
        thread::sleep(Duration::from_millis(5));
    }
    last
}

fn wait_for_argv(record: &Path) -> Vec<String> {
    for _ in 0..100 {
        if record.exists() {
            return read_argv(record);
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("argv record was not written: {}", record.display());
}

fn scene_players(scratch: &Path, plasmashell: PathBuf, data_home: PathBuf) -> Players {
    let stubs = stubs_in(scratch);
    Players {
        video: stubs.video,
        web: stubs.web,
        plasma: false,
        web_plasma: false,
        plasmashell,
        plasma_data_home: data_home,
        muted: true,
    }
}

#[test]
fn library_live_scene_is_false_without_the_plugin_and_play_stays_partial() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("steam-root");
    let project = workshop_path(&steam_root).join("1001");
    write_project(
        &project,
        r#"{"type":"scene","file":"scene.json","title":"Synthetic Scene"}"#,
    );
    write_file(
        &project.join("scene.json"),
        r#"{"objects":[{"classname":"ImageLayer","image":"layer.png","visible":true}]}"#,
    );
    let image = project.join("layer.png");
    write_file(&image, "synthetic png bytes");

    let search = scratch.join("empty-plasma-search");
    fs::create_dir_all(&search).expect("empty search dir");
    let data_home = scratch.join("data-home");
    let plasma_record = scratch.join("plasma-argv");
    let plasmashell = write_argv_stub(&scratch, "qdbus", &plasma_record);
    let players = scene_players(&scratch, plasmashell, data_home.clone());
    let video_record = players.video.with_file_name("video-argv");
    let web_record = players.web.with_file_name("web-argv");
    let server = serve_options(DesktopOptions {
        steam_root: Some(steam_root),
        home: Some(scratch.join("home")),
        config_home: scratch.join("config"),
        players,
        plasma_data_dirs: Some(vec![search]),
        version: "0.1.0".to_string(),
        releases_url: None,
        install_dir: Some(scratch.join("install")),
    });

    let (status, body) = get(server.port(), "/api/library");
    assert_eq!(status, 200, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("library json");
    assert_eq!(payload["liveScene"], false);
    let scene = payload["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["id"] == "1001")
        .expect("scene");
    assert_eq!(scene["type"], "scene");

    let (status, body) = post_play(server.port(), "1001");
    assert_eq!(status, 200, "{body}");
    let script = wait_for_argv(&plasma_record)
        .into_iter()
        .next_back()
        .expect("script");
    assert!(script.contains(PLASMA_SCENE_WALLPAPER_PLUGIN), "{script}");
    assert!(script.contains("layer.png"), "{script}");
    assert!(!script.contains(LIVE_SCENE_PLUGIN_ID), "{script}");
    assert!(
        plasma_scene_wallpaper_dir(&data_home)
            .join("metadata.json")
            .is_file(),
        "partial scene play did not install the static plugin"
    );
    assert!(!video_record.exists(), "partial scene play spawned video");
    assert!(!web_record.exists(), "partial scene play spawned web");

    drop(server);
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn library_live_scene_is_true_when_the_plugin_directory_exists() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("steam-root");
    let project = workshop_path(&steam_root).join("1001");
    write_project(
        &project,
        r#"{"type":"scene","file":"scene.pkg","title":"Packed Scene"}"#,
    );
    write_file(&project.join("scene.pkg"), "synthetic scene package");

    let search = scratch.join("plasma-search");
    install_live_package(&search);
    let data_home = scratch.join("data-home");
    let plasma_record = scratch.join("plasma-argv");
    let plasmashell = write_argv_stub(&scratch, "qdbus", &plasma_record);
    let players = scene_players(&scratch, plasmashell, data_home.clone());
    let video_record = players.video.with_file_name("video-argv");
    let web_record = players.web.with_file_name("web-argv");
    let server = serve_options(DesktopOptions {
        steam_root: Some(steam_root),
        home: Some(scratch.join("home")),
        config_home: scratch.join("config"),
        players,
        plasma_data_dirs: Some(vec![search]),
        version: "0.1.0".to_string(),
        releases_url: None,
        install_dir: Some(scratch.join("install")),
    });

    let (status, body) = get(server.port(), "/api/library");
    assert_eq!(status, 200, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("library json");
    assert_eq!(payload["liveScene"], true);

    let (status, body) = post_play(server.port(), "1001");
    assert_eq!(status, 200, "{body}");
    let script = wait_for_argv(&plasma_record)
        .into_iter()
        .next_back()
        .expect("script");
    assert!(script.contains(LIVE_SCENE_PLUGIN_ID), "{script}");
    assert!(script.contains(&project.display().to_string()), "{script}");
    assert!(!script.contains(PLASMA_SCENE_WALLPAPER_PLUGIN), "{script}");
    assert!(
        !plasma_scene_wallpaper_dir(&data_home).exists(),
        "live scene play installed the static plugin"
    );
    assert!(!video_record.exists(), "live scene play spawned video");
    assert!(!web_record.exists(), "live scene play spawned web");

    drop(server);
    let _ = fs::remove_dir_all(&scratch);
}

fn post_empty(port: u16, path: &str) -> (u16, String) {
    exchange(
        port,
        &format!("POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"),
    )
}

struct Fixture {
    port: u16,
    stop: Arc<AtomicBool>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

fn bind_fixture() -> (TcpListener, u16) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("fixture bind");
    let port = listener.local_addr().expect("fixture port").port();
    (listener, port)
}

fn serve_fixture(listener: TcpListener, routes: HashMap<String, Vec<u8>>) -> Fixture {
    serve_fixture_with(listener, routes, Duration::from_millis(0))
}

fn serve_fixture_with(
    listener: TcpListener,
    routes: HashMap<String, Vec<u8>>,
    delay: Duration,
) -> Fixture {
    let port = listener.local_addr().expect("fixture port").port();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    thread::spawn(move || {
        while !flag.load(Ordering::SeqCst) {
            let mut stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(_) => continue,
            };
            if flag.load(Ordering::SeqCst) {
                break;
            }
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(count) => {
                        buf.extend_from_slice(&chunk[..count]);
                        if buf.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let text = String::from_utf8_lossy(&buf);
            let path = text
                .split_whitespace()
                .nth(1)
                .unwrap_or("/")
                .split('?')
                .next()
                .unwrap_or("/");
            if !delay.is_zero() {
                thread::sleep(delay);
            }
            let body = routes.get(path).cloned().unwrap_or_default();
            let status = if routes.contains_key(path) { 200 } else { 404 };
            let header = format!(
                "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        }
    });
    Fixture { port, stop }
}

fn release_json(items: &[serde_json::Value]) -> Vec<u8> {
    serde_json::Value::Array(items.to_vec())
        .to_string()
        .into_bytes()
}

fn release_item(
    tag: &str,
    prerelease: bool,
    published: &str,
    notes: &str,
    port: u16,
) -> serde_json::Value {
    serde_json::json!({
        "tag_name": tag,
        "prerelease": prerelease,
        "draft": false,
        "body": notes,
        "published_at": published,
        "assets": [
            {
                "name": ARCHIVE_NAME,
                "browser_download_url": format!("http://127.0.0.1:{port}/{ARCHIVE_NAME}")
            },
            {
                "name": SUMS_NAME,
                "browser_download_url": format!("http://127.0.0.1:{port}/{SUMS_NAME}")
            }
        ]
    })
}

#[test]
fn update_channel_defaults_to_release_and_survives_other_settings() {
    let scratch = scratch_dir();
    let config = scratch.join("config");
    let fresh = load_settings(&config).expect("defaults");
    assert_eq!(fresh.update_channel, UpdateChannel::Release);

    let preview = set_update_channel(&config, UpdateChannel::Preview).expect("preview");
    assert_eq!(preview.update_channel, UpdateChannel::Preview);
    assert_eq!(
        fs::read_to_string(settings_path(&config)).expect("preview file"),
        "{\"updateChannel\":\"preview\"}\n"
    );

    set_autostart(&config, false).expect("decline");
    let text = fs::read_to_string(settings_path(&config)).expect("kept channel");
    assert_eq!(
        text,
        "{\"autostartAsked\":true,\"autostart\":false,\"updateChannel\":\"preview\"}\n"
    );

    fs::write(settings_path(&config), "{\"updateChannel\":\"beta\"}\n").expect("bad channel");
    let loaded = load_settings(&config).expect("unknown channel still loads");
    assert_eq!(loaded.update_channel, UpdateChannel::Release);

    set_update_channel(&config, UpdateChannel::Release).expect("back to release");
    let text = fs::read_to_string(settings_path(&config)).expect("release omitted");
    assert!(!text.contains("updateChannel"), "{text}");

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn wallpaper_version_env_overrides_the_crate_version() {
    let scratch = scratch_dir();
    let config = scratch.join("config");
    let steam_root = scratch.join("steam-root");
    fs::create_dir_all(&steam_root).expect("steam root");
    let steam_root = steam_root.display().to_string();
    let mut child = Command::new(desktop_bin())
        .args(["--steam-root", &steam_root])
        .env("HOME", &scratch)
        .env("XDG_CONFIG_HOME", &config)
        .env("WALLPAPER_VERSION", "9.9.9")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wallpaper desktop");
    let stdout = child.stdout.take().expect("stdout");
    let (sender, receiver) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stdout);
        let mut line = String::new();
        let _ = std::io::BufRead::read_line(&mut reader, &mut line);
        let _ = sender.send(line);
    });
    let line = receiver.recv_timeout(Duration::from_secs(15)).expect("url");
    let port: u16 = line
        .trim()
        .trim_start_matches("http://127.0.0.1:")
        .parse()
        .expect("port");
    let running = RunningDesktop { child, port };
    let (status, body) = get(running.port, "/api/settings");
    assert_eq!(status, 200, "{body}");
    let settings: serde_json::Value = serde_json::from_str(&body).expect("settings");
    assert_eq!(settings["version"], "9.9.9");
    assert_eq!(settings["updateChannel"], "release");
}

#[test]
fn settings_stay_up_while_an_update_check_is_stuck() {
    let scratch = scratch_dir();
    let (listener, _) = bind_fixture();
    let mut routes = HashMap::new();
    routes.insert("/releases".to_string(), b"[]".to_vec());
    let fixture = serve_fixture_with(listener, routes, Duration::from_secs(3));
    let mut desktop = options(&scratch, scratch.join("steam-root"), Players::default());
    desktop.releases_url = Some(format!("http://127.0.0.1:{}/releases", fixture.port));
    let server = serve_options(desktop);
    let port = server.port();
    let checker = thread::spawn(move || post_empty(port, "/api/updates/check"));

    let started = Instant::now();
    let (status, body) = get(server.port(), "/api/settings");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "settings waited on the update check"
    );
    assert_eq!(status, 200, "{body}");
    let settings: serde_json::Value = serde_json::from_str(&body).expect("settings");
    assert_eq!(settings["autostartAsked"], false);
    assert_eq!(settings["updateChannel"], "release");

    let (status, body) = checker.join().expect("check thread");
    assert_eq!(status, 200, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("check");
    assert_eq!(payload["state"], "none");
    drop(server);
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn github_channels_offer_a_newer_build_and_keep_a_verified_download() {
    let scratch = scratch_dir();
    let archive = pack_gzip_tar(&[
        ("bundle/wallpaper", b"synthetic-wallpaper"),
        ("bundle/wallpaper-desktop", b"synthetic-desktop"),
    ])
    .expect("pack");
    let digest = sha256_hex(&archive);
    let sums = format!("{digest}  {ARCHIVE_NAME}\n");
    let (listener, fixture_port) = bind_fixture();
    let mut routes = HashMap::new();
    routes.insert(
        "/releases".to_string(),
        release_json(&[
            release_item(
                "vanilla",
                false,
                "2026-09-06T00:00:00Z",
                "ignore",
                fixture_port,
            ),
            release_item(
                "v0.9.0",
                true,
                "2026-09-05T00:00:00Z",
                "prerelease v",
                fixture_port,
            ),
            release_item(
                "v0.2.0",
                false,
                "2026-09-01T00:00:00Z",
                "Stable playback.",
                fixture_port,
            ),
            release_item(
                "v0.8.0",
                false,
                "2026-08-01T00:00:00Z",
                "older",
                fixture_port,
            ),
            release_item(
                "preview-0.3.0",
                true,
                "2026-09-03T00:00:00Z",
                "Preview note.",
                fixture_port,
            ),
            release_item(
                "preview-0.4.0",
                false,
                "2026-09-04T00:00:00Z",
                "not preview",
                fixture_port,
            ),
            release_item(
                "nightly-0.4.0",
                true,
                "2026-09-04T00:00:00Z",
                "Nightly note.",
                fixture_port,
            ),
            release_item(
                "nightly-9.0.0",
                false,
                "2026-09-07T00:00:00Z",
                "not nightly",
                fixture_port,
            ),
        ]),
    );
    routes.insert(format!("/{ARCHIVE_NAME}"), archive);
    routes.insert(format!("/{SUMS_NAME}"), sums.into_bytes());
    let fixture = serve_fixture(listener, routes);

    let install = scratch.join("install");
    fs::create_dir_all(&install).expect("install");
    let mut desktop = options(&scratch, scratch.join("steam-root"), Players::default());
    desktop.version = "0.1.0".to_string();
    desktop.install_dir = Some(install.clone());
    desktop.releases_url = Some(format!("http://127.0.0.1:{}/releases", fixture.port));
    let server = serve_options(desktop);
    let port = server.port();

    let (status, body) = post_empty(port, "/api/updates/download");
    assert_eq!(status, 200, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("early download");
    assert_eq!(payload["ok"], false);
    assert!(payload["error"].as_str().unwrap_or("").contains("Check"));
    assert!(!install.join("wallpaper").exists());

    let (status, body) = post_empty(port, "/api/updates/check");
    assert_eq!(status, 200, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("release check");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["state"], "available");
    assert_eq!(payload["channel"], "release");
    assert_eq!(payload["current"], "0.1.0");
    assert_eq!(payload["version"], "v0.2.0");
    assert_eq!(payload["notes"], "Stable playback.");

    let (status, body) = post_empty(port, "/api/updates/download");
    assert_eq!(status, 200, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("download");
    assert_eq!(payload["ok"], true, "{body}");
    assert_eq!(payload["state"], "installed");
    assert_eq!(payload["message"], NEXT_LAUNCH_MESSAGE);
    assert_eq!(
        fs::read(install.join("wallpaper")).expect("wallpaper"),
        b"synthetic-wallpaper"
    );
    assert_eq!(
        fs::read(install.join("wallpaper-desktop")).expect("desktop"),
        b"synthetic-desktop"
    );
    assert!(!scratch.join("config/wallpaper").join(ARCHIVE_NAME).exists());

    let (status, body) = post_json(port, "/api/updates/channel", r#"{"channel":"preview"}"#);
    assert_eq!(status, 200, "{body}");
    let (status, body) = post_empty(port, "/api/updates/check");
    assert_eq!(status, 200, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("preview");
    assert_eq!(payload["version"], "preview-0.3.0");
    assert_eq!(payload["notes"], "Preview note.");
    assert_eq!(payload["channel"], "preview");

    let (status, body) = post_json(port, "/api/updates/channel", r#"{"channel":"nightly"}"#);
    assert_eq!(status, 200, "{body}");
    let (status, body) = post_empty(port, "/api/updates/check");
    assert_eq!(status, 200, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("nightly");
    assert_eq!(payload["version"], "nightly-0.4.0");
    assert_eq!(payload["notes"], "Nightly note.");

    let saved = fs::read_to_string(settings_path(&scratch.join("config"))).expect("settings");
    assert!(saved.contains("\"updateChannel\":\"nightly\""), "{saved}");

    let (status, body) = post_json(port, "/api/updates/channel", r#"{"channel":"beta"}"#);
    assert_eq!(status, 400, "{body}");
    let still = fs::read_to_string(settings_path(&scratch.join("config"))).expect("unchanged");
    assert!(still.contains("\"updateChannel\":\"nightly\""), "{still}");

    drop(server);
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn a_failed_hash_or_closed_channel_is_visible_and_keeps_nothing() {
    let scratch = scratch_dir();
    let archive = pack_gzip_tar(&[
        ("wallpaper", b"synthetic-wallpaper"),
        ("wallpaper-desktop", b"synthetic-desktop"),
    ])
    .expect("pack");
    let (listener, fixture_port) = bind_fixture();
    let mut routes = HashMap::new();
    routes.insert(
        "/releases".to_string(),
        release_json(&[release_item(
            "v0.4.0",
            false,
            "2026-09-01T00:00:00Z",
            "Bad hash.",
            fixture_port,
        )]),
    );
    routes.insert(format!("/{ARCHIVE_NAME}"), archive);
    routes.insert(
        format!("/{SUMS_NAME}"),
        format!(
            "0000000000000000000000000000000000000000000000000000000000000000  {ARCHIVE_NAME}\n"
        )
        .into_bytes(),
    );
    let fixture = serve_fixture(listener, routes);
    let install = scratch.join("install");
    fs::create_dir_all(&install).expect("install");
    let mut desktop = options(&scratch, scratch.join("steam-root"), Players::default());
    desktop.install_dir = Some(install.clone());
    desktop.releases_url = Some(format!("http://127.0.0.1:{}/releases", fixture.port));
    let server = serve_options(desktop);
    let port = server.port();

    let (status, body) = post_empty(port, "/api/updates/check");
    assert_eq!(status, 200, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("offer");
    assert_eq!(payload["state"], "available");
    assert_eq!(payload["version"], "v0.4.0");

    let (status, body) = post_empty(port, "/api/updates/download");
    assert_eq!(status, 200, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("hash");
    assert_eq!(payload["ok"], false);
    assert!(
        payload["error"]
            .as_str()
            .unwrap_or("")
            .contains("sha256sums.txt"),
        "{body}"
    );
    assert!(!install.join("wallpaper").exists());
    assert!(!scratch.join("config/wallpaper").join(ARCHIVE_NAME).exists());
    assert!(files_named(&scratch.join("config"), ARCHIVE_NAME).is_empty());

    drop(server);
    drop(fixture);

    let mut desktop = options(&scratch, scratch.join("steam-root"), Players::default());
    desktop.releases_url = Some("http://127.0.0.1:1/releases".to_string());
    let server = serve_options(desktop);
    let (status, body) = post_empty(server.port(), "/api/updates/check");
    assert_eq!(status, 200, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("network");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["state"], "error");
    assert_eq!(payload["error"], "Could not check for updates.");

    let (status, body) = get(server.port(), "/api/library");
    assert_eq!(status, 200, "{body}");
    drop(server);
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn an_unwritable_install_dir_saves_the_archive_and_names_the_path() {
    let scratch = scratch_dir();
    let archive = pack_gzip_tar(&[
        ("wallpaper", b"synthetic-wallpaper"),
        ("wallpaper-desktop", b"synthetic-desktop"),
    ])
    .expect("pack");
    let digest = sha256_hex(&archive);
    let (listener, fixture_port) = bind_fixture();
    let mut routes = HashMap::new();
    routes.insert(
        "/releases".to_string(),
        release_json(&[release_item(
            "v1.2.0",
            false,
            "2026-09-08T00:00:00Z",
            "Saved aside.",
            fixture_port,
        )]),
    );
    routes.insert(format!("/{ARCHIVE_NAME}"), archive.clone());
    routes.insert(
        format!("/{SUMS_NAME}"),
        format!("{digest}  {ARCHIVE_NAME}\n").into_bytes(),
    );
    let fixture = serve_fixture(listener, routes);
    let install = scratch.join("locked-install");
    fs::create_dir_all(&install).expect("install");
    let mut permissions = fs::metadata(&install).expect("meta").permissions();
    permissions.set_mode(0o555);
    fs::set_permissions(&install, permissions).expect("chmod");

    let mut desktop = options(&scratch, scratch.join("steam-root"), Players::default());
    desktop.install_dir = Some(install.clone());
    desktop.releases_url = Some(format!("http://127.0.0.1:{}/releases", fixture.port));
    desktop.version = "0.1.0".to_string();
    let server = serve_options(desktop);

    let (status, body) = post_empty(server.port(), "/api/updates/check");
    assert_eq!(status, 200, "{body}");
    let (status, body) = post_empty(server.port(), "/api/updates/download");
    assert_eq!(status, 200, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("saved");
    assert_eq!(payload["ok"], true, "{body}");
    assert_eq!(payload["state"], "saved");
    let path = scratch.join("config/wallpaper").join(ARCHIVE_NAME);
    assert_eq!(payload["path"], path.display().to_string());
    assert!(payload["message"]
        .as_str()
        .unwrap_or("")
        .contains(&path.display().to_string()));
    assert_eq!(fs::read(&path).expect("kept archive"), archive);
    assert!(!install.join("wallpaper").exists());

    let mut desktop = options(&scratch, scratch.join("steam-root"), Players::default());
    desktop.version = "9.0.0".to_string();
    desktop.releases_url = Some(format!("http://127.0.0.1:{}/releases", fixture.port));
    let current = serve_options(desktop);
    let (status, body) = post_empty(current.port(), "/api/updates/check");
    assert_eq!(status, 200, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("current");
    assert_eq!(payload["state"], "current");
    assert_eq!(payload["version"], "v1.2.0");

    let mut desktop = options(&scratch, scratch.join("steam-root"), Players::default());
    desktop.releases_url = Some(format!("http://127.0.0.1:{}/releases", fixture.port));
    let nightly = serve_options(desktop);
    let (status, body) = post_json(
        nightly.port(),
        "/api/updates/channel",
        r#"{"channel":"nightly"}"#,
    );
    assert_eq!(status, 200, "{body}");
    let (status, body) = post_empty(nightly.port(), "/api/updates/check");
    assert_eq!(status, 200, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("empty nightly");
    assert_eq!(payload["state"], "none");
    assert_eq!(payload["error"], "No release on the nightly channel.");

    drop(nightly);
    drop(current);
    let mut permissions = fs::metadata(&install).expect("meta").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&install, permissions).expect("restore");
    let _ = fs::remove_dir_all(&scratch);
}
