use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wallpaper_desktop::{
    autostart_desktop_path, collect_library, load_settings, serve, set_autostart,
    set_extra_library, settings_path, DesktopOptions, Server,
};
use wallpaper_play::Players;

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
    DesktopOptions {
        steam_root: Some(steam_root),
        home: Some(scratch.join("home")),
        config_home: scratch.join("config"),
        players,
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
    match running.child.try_wait() {
        Ok(None) => {}
        Ok(Some(status)) => panic!("desktop exited without a display: {status}"),
        Err(error) => panic!("wait failed: {error}"),
    }

    let _ = fs::remove_dir_all(&scratch);
}
