use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

fn scratch_dir() -> PathBuf {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("wallpaper-ui-{nanos}-{seq}"));
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

fn write_scene(library: &Path, id: &str) {
    write_project(
        &library.join(id),
        r#"{"type":"scene","file":"scene.json","title":"Synthetic Scene"}"#,
    );
}

fn write_application(library: &Path, id: &str) {
    write_project(
        &library.join(id),
        r#"{"type":"application","file":"app.exe","title":"Synthetic App"}"#,
    );
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

fn write_web(library: &Path, id: &str, file_name: &str) -> PathBuf {
    let project = library.join(id);
    write_project(
        &project,
        &format!(r#"{{"type":"web","file":"{file_name}","title":"Synthetic Web"}}"#),
    );
    let html = project.join(file_name);
    write_file(&html, "<!doctype html><title>Synthetic Web</title>");
    html
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

struct RunningUi {
    child: Child,
    port: u16,
}

impl Drop for RunningUi {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_ui(home: &Path, args: &[&str]) -> RunningUi {
    start_ui_with_data_dirs(home, home, args)
}

fn start_ui_with_data_dirs(home: &Path, data_dirs: &Path, args: &[&str]) -> RunningUi {
    let mut child = Command::new(env!("CARGO_BIN_EXE_wallpaper"))
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join("xdg-data"))
        .env("XDG_DATA_DIRS", home.join("xdg-data"))
        .env("WALLPAPER_PLASMA_DATA_DIRS", data_dirs)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wallpaper ui");
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
        .expect("wallpaper ui did not print a url");
    let url = line.trim();
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "expected a loopback url, got {url}"
    );
    let port: u16 = url
        .trim_start_matches("http://127.0.0.1:")
        .parse()
        .expect("port");
    assert!(
        port != 3000 && port != 5173 && port != 8080,
        "ui bound a reserved port: {port}"
    );
    RunningUi { child, port }
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

fn library_with_one_of_each(scratch: &Path) -> (PathBuf, PathBuf) {
    let steam_root = scratch.join("steam-root");
    let library = workshop_path(&steam_root);
    write_scene(&library, "1001");
    write_application(&library, "1002");
    write_video(&library, "1003", "wallpaper.mp4");
    write_web(&library, "1004", "index.html");
    (steam_root, library)
}

#[test]
fn library_api_returns_the_video_item() {
    let scratch = scratch_dir();
    let (steam_root, library) = library_with_one_of_each(&scratch);
    let stubs = stubs_in(&scratch);
    let steam_root = steam_root.display().to_string();
    let video_player = stubs.video.display().to_string();
    let web_player = stubs.web.display().to_string();
    let ui = start_ui(
        &scratch,
        &[
            "ui",
            "--steam-root",
            &steam_root,
            "--video-player",
            &video_player,
            "--web-player",
            &web_player,
        ],
    );

    let (status, body) = get(ui.port, "/api/library");
    assert_eq!(status, 200, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("library json");
    assert_eq!(payload["found"], true);
    assert_eq!(payload["path"], library.display().to_string());
    assert_eq!(payload["liveScene"], false);
    let items = payload["items"].as_array().expect("items");
    let video = items
        .iter()
        .find(|item| item["id"] == "1003")
        .expect("video item");
    assert_eq!(video["type"], "video");
    assert_eq!(video["title"], "Synthetic Video");
    let scene = items
        .iter()
        .find(|item| item["id"] == "1001")
        .expect("scene stays in the list");
    assert_eq!(scene["type"], "scene");
    assert_eq!(scene["title"], "Synthetic Scene");
    assert!(items.iter().any(|item| item["type"] == "web"));
    assert!(items.iter().any(|item| item["type"] == "application"));
    assert!(
        !stubs.video_record.exists(),
        "listing spawned the video player"
    );
    assert!(!stubs.web_record.exists(), "listing spawned the web player");
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn play_scene_returns_an_error_and_does_not_spawn() {
    let scratch = scratch_dir();
    let (steam_root, _) = library_with_one_of_each(&scratch);
    let stubs = stubs_in(&scratch);
    let steam_root = steam_root.display().to_string();
    let video_player = stubs.video.display().to_string();
    let web_player = stubs.web.display().to_string();
    let ui = start_ui(
        &scratch,
        &[
            "ui",
            "--steam-root",
            &steam_root,
            "--video-player",
            &video_player,
            "--web-player",
            &web_player,
        ],
    );

    let (status, body) = post_play(ui.port, "1001");
    assert!(status >= 400, "scene play succeeded: {status} {body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("play json");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["id"], "1001");
    let error = payload["error"].as_str().expect("error text");
    assert!(error.contains("1001"), "error did not name the id: {error}");
    thread::sleep(Duration::from_millis(150));
    assert!(
        !stubs.video_record.exists(),
        "scene play spawned the video player"
    );
    assert!(
        !stubs.web_record.exists(),
        "scene play spawned the web player"
    );
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn play_video_spawns_the_stub_player() {
    let scratch = scratch_dir();
    let (steam_root, _) = library_with_one_of_each(&scratch);
    let media = workshop_path(Path::new(&steam_root)).join("1003/wallpaper.mp4");
    let stubs = stubs_in(&scratch);
    let steam_root = steam_root.display().to_string();
    let video_player = stubs.video.display().to_string();
    let web_player = stubs.web.display().to_string();
    let ui = start_ui(
        &scratch,
        &[
            "ui",
            "--steam-root",
            &steam_root,
            "--video-player",
            &video_player,
            "--web-player",
            &web_player,
        ],
    );

    let (status, body) = post_play(ui.port, "1003");
    assert_eq!(status, 200, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("play json");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["id"], "1003");
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
            media.display().to_string(),
        ]
    );
    assert!(
        !stubs.web_record.exists(),
        "video play spawned the web player"
    );
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn play_web_spawns_the_stub_player() {
    let scratch = scratch_dir();
    let (steam_root, library) = library_with_one_of_each(&scratch);
    let html = library.join("1004/index.html");
    let stubs = stubs_in(&scratch);
    let steam_root = steam_root.display().to_string();
    let video_player = stubs.video.display().to_string();
    let web_player = stubs.web.display().to_string();
    let ui = start_ui(
        &scratch,
        &[
            "ui",
            "--steam-root",
            &steam_root,
            "--video-player",
            &video_player,
            "--web-player",
            &web_player,
        ],
    );

    let (status, body) = post_play(ui.port, "1004");
    assert_eq!(status, 200, "{body}");
    for _ in 0..100 {
        if stubs.web_record.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        read_argv(&stubs.web_record),
        vec![
            stubs.web.display().to_string(),
            format!("file://{}", html.display()),
        ]
    );
    assert!(
        !stubs.video_record.exists(),
        "web play spawned the video player"
    );
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn missing_library_reports_none_and_does_not_spawn() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("empty-steam");
    fs::create_dir_all(&steam_root).expect("empty steam root");
    let home = scratch.join("home");
    write_video(
        &workshop_path(&home.join(".steam/steam")),
        "1001",
        "from-home.mp4",
    );
    let stubs = stubs_in(&scratch);
    let steam_root = steam_root.display().to_string();
    let video_player = stubs.video.display().to_string();
    let web_player = stubs.web.display().to_string();
    let ui = start_ui(
        &home,
        &[
            "ui",
            "--steam-root",
            &steam_root,
            "--video-player",
            &video_player,
            "--web-player",
            &web_player,
        ],
    );

    let (status, body) = get(ui.port, "/api/library");
    assert_eq!(status, 200, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("library json");
    assert_eq!(payload["found"], false);
    assert!(payload["items"].as_array().expect("items").is_empty());
    let (play_status, play_body) = post_play(ui.port, "1001");
    assert!(play_status >= 400, "{play_body}");
    assert!(play_body.contains("1001"));
    thread::sleep(Duration::from_millis(100));
    assert!(
        !stubs.video_record.exists(),
        "missing library spawned a player"
    );
    assert!(
        !stubs.web_record.exists(),
        "missing library spawned a player"
    );
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn page_includes_the_library_hooks() {
    let scratch = scratch_dir();
    let (steam_root, _) = library_with_one_of_each(&scratch);
    let steam_root = steam_root.display().to_string();
    let ui = start_ui(&scratch, &["ui", "--steam-root", &steam_root]);

    let (status, html) = get(ui.port, "/");
    assert_eq!(status, 200, "{html}");
    assert!(html.contains("id=\"status\""));
    assert!(html.contains("id=\"library\""));
    assert!(html.contains("id=\"error\""));
    assert!(html.contains("hidden"));
    assert!(html.contains("ui/app.js"));
    assert!(html.contains("ui/styles.css"));

    let (script_status, script) = get(ui.port, "/ui/app.js");
    assert_eq!(script_status, 200, "{script}");
    assert!(script.contains("/api/library"));
    assert!(script.contains("/api/play"));
    assert!(script.contains("No workshop items found."));
    assert!(script.contains("wallpaper-card"));
    assert!(script.contains("data-id") || script.contains("dataset.id"));
    assert!(script.contains("payload.liveScene === true"));
    assert!(script.contains("type === \"scene\" && liveScene === true"));
    assert!(script.contains("button.disabled = true"));
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn library_reports_the_live_plugin_and_play_selects_it() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("steam-root");
    let library = workshop_path(&steam_root);
    let project = library.join("1001");
    write_project(
        &project,
        r#"{"type":"scene","file":"scene.pkg","title":"Packed Scene"}"#,
    );
    write_file(&project.join("scene.pkg"), "synthetic scene package");
    write_video(&library, "1003", "wallpaper.mp4");

    let search = scratch.join("plasma-search");
    fs::create_dir_all(search.join("plasma/wallpapers/com.github.captsilver.wallpaperEngineKde"))
        .expect("live package");
    let plasma_record = scratch.join("plasma-argv");
    let plasmashell = write_argv_stub(&scratch, "qdbus6", &plasma_record);
    let stubs = stubs_in(&scratch);
    let steam_root = steam_root.display().to_string();
    let plasmashell = plasmashell.display().to_string();
    let video_player = stubs.video.display().to_string();
    let web_player = stubs.web.display().to_string();
    let ui = start_ui_with_data_dirs(
        &scratch,
        &search,
        &[
            "ui",
            "--steam-root",
            &steam_root,
            "--plasmashell",
            &plasmashell,
            "--video-player",
            &video_player,
            "--web-player",
            &web_player,
        ],
    );

    let (status, body) = get(ui.port, "/api/library");
    assert_eq!(status, 200, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("library json");
    assert_eq!(payload["liveScene"], true);
    let scene = payload["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["id"] == "1001")
        .expect("scene");
    assert_eq!(scene["type"], "scene");

    let (status, body) = post_play(ui.port, "1001");
    assert_eq!(status, 200, "{body}");
    for _ in 0..100 {
        if plasma_record.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let script = read_argv(&plasma_record)
        .into_iter()
        .next_back()
        .expect("script");
    assert!(
        script.contains("com.github.captsilver.wallpaperEngineKde"),
        "{script}"
    );
    assert!(script.contains(&project.display().to_string()), "{script}");
    assert!(!script.contains("linux.wallpaper.scene"), "{script}");
    assert!(
        !scratch
            .join("xdg-data/plasma/wallpapers/linux.wallpaper.scene")
            .exists(),
        "api play installed the static scene plugin"
    );
    thread::sleep(Duration::from_millis(50));
    assert!(!stubs.video_record.exists(), "scene play spawned mpv");
    assert!(
        !stubs.web_record.exists(),
        "scene play spawned the web player"
    );
    let _ = fs::remove_dir_all(&scratch);
}

fn css_rules(css: &str) -> Vec<(String, String)> {
    let mut cleaned = String::new();
    let mut chars = css.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            while let Some(inner) = chars.next() {
                if inner == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    break;
                }
            }
            continue;
        }
        cleaned.push(ch);
    }
    cleaned
        .split('}')
        .filter_map(|chunk| {
            let (selector, body) = chunk.split_once('{')?;
            let selector = selector.trim();
            if selector.is_empty() || selector.starts_with('@') {
                return None;
            }
            Some((selector.to_string(), body.to_string()))
        })
        .collect()
}

fn assert_preview_color_contract(css: &str) {
    let rules = css_rules(css);
    let mut resting = false;
    let mut hover = false;
    let mut focus = false;
    for (selector, body) in &rules {
        let selectors: Vec<&str> = selector.split(',').map(str::trim).collect();
        let opens_color = body.contains("grayscale(0)")
            || body.contains("filter: none")
            || body.contains("filter:none");
        if opens_color {
            for sel in &selectors {
                assert!(
                    *sel == ".wallpaper-card:hover .preview"
                        || *sel == ".wallpaper-card:focus-visible .preview",
                    "color is only allowed on card hover and focus-visible, got `{sel}`"
                );
                if *sel == ".wallpaper-card:hover .preview" {
                    hover = true;
                }
                if *sel == ".wallpaper-card:focus-visible .preview" {
                    focus = true;
                }
            }
        }
        if selectors
            .iter()
            .any(|sel| *sel == ".wallpaper-card .preview")
        {
            assert!(
                body.contains("grayscale(1)"),
                "resting previews stay grayscale"
            );
            assert!(
                !body.contains("grayscale(0)"),
                "resting previews do not reveal color"
            );
            resting = true;
        }
    }
    assert!(resting, "missing the resting grayscale preview rule");
    assert!(hover, "missing the hover color rule");
    assert!(focus, "missing the focus-visible color rule");
    assert!(
        !css.contains("hue-rotate") && !css.contains("saturate("),
        "the chrome does not recolor from sampled accents"
    );
}

fn function_body<'a>(script: &'a str, name: &str) -> &'a str {
    let marker = format!("function {name}(");
    let start = script
        .find(&marker)
        .unwrap_or_else(|| panic!("missing {name}"));
    let bytes = script.as_bytes();
    let mut index = start;
    while index < bytes.len() && bytes[index] != b'{' {
        index += 1;
    }
    let mut depth = 0;
    let mut quote: Option<u8> = None;
    let mut escape = false;
    let body_start = index;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(open) = quote {
            if escape {
                escape = false;
            } else if byte == b'\\' {
                escape = true;
            } else if byte == open {
                quote = None;
            }
        } else {
            match byte {
                b'"' | b'\'' | b'`' => quote = Some(byte),
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &script[body_start..=index];
                    }
                }
                _ => {}
            }
        }
        index += 1;
    }
    panic!("unclosed function {name}");
}

#[test]
fn preview_color_is_only_on_hover_and_focus_visible() {
    let scratch = scratch_dir();
    let (steam_root, _) = library_with_one_of_each(&scratch);
    let steam_root = steam_root.display().to_string();
    let ui = start_ui(&scratch, &["ui", "--steam-root", &steam_root]);

    let (status, css) = get(ui.port, "/ui/styles.css");
    assert_eq!(status, 200, "{css}");
    assert_preview_color_contract(&css);

    let (status, html) = get(ui.port, "/");
    assert_eq!(status, 200, "{html}");
    assert!(html.contains("<div id=\"updates\" hidden>"));
    assert!(html.contains("value=\"release\""));
    assert!(html.contains("value=\"preview\""));
    assert!(html.contains("value=\"nightly\""));
    assert!(html.contains("id=\"update-download\""));

    let (status, script) = get(ui.port, "/ui/app.js");
    assert_eq!(status, 200, "{script}");
    assert!(script.contains("card.tabIndex = 0"));
    let settings = function_body(&script, "loadSettings");
    let prompt = settings
        .find("autostartPromptEl.hidden = false")
        .expect("prompt");
    let check = settings.find("checkUpdates()").expect("startup check");
    assert!(
        prompt < check,
        "the login question is shown before the update check starts"
    );
    assert!(!function_body(&script, "checkUpdates").contains("/api/updates/download"));
    assert!(function_body(&script, "downloadUpdate").contains("/api/updates/download"));
    let _ = fs::remove_dir_all(&scratch);
}
