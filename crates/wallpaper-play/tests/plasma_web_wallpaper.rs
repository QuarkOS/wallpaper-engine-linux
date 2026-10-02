use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wallpaper_import::scan_library;
use wallpaper_play::{
    install_plasma_video_wallpaper, install_plasma_web_wallpaper, launch, plan_playback,
    plasma_wallpaper_dir, plasma_web_wallpaper_dir, plasma_web_wallpaper_script, LaunchError,
    PlayPlan, Players, PLASMA_DBUS_METHOD, PLASMA_DBUS_PATH, PLASMA_DBUS_SERVICE,
    PLASMA_VIDEO_WALLPAPER_PLUGIN, PLASMA_WEB_WALLPAPER_PLUGIN,
};

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

fn scratch_dir() -> PathBuf {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("wallpaper-plasma-web-{nanos}-{seq}"));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn write_file(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, body).expect("write file");
}

/// The committed web fixture names `index.html` and does not ship that file.
/// Copy the project and write a synthetic page next to it.
fn write_web_project(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).expect("create project");
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../wallpaper-import/fixtures/web/project.json");
    fs::copy(&fixture, dir.join("project.json")).expect("copy web fixture");
    let html = dir.join("index.html");
    write_file(&html, "<!doctype html><title>Synthetic Web</title>");
    html
}

fn planned_web(root: &Path) -> (PathBuf, PlayPlan) {
    write_web_project(&root.join("synthetic-web"));
    let library = scan_library(root).expect("player scans the library");
    assert_eq!(library.len(), 1, "player sees one project");
    let plan = wallpaper_play::plan_playback(&library[0]).expect("web plan");
    let PlayPlan::Web { ref url } = plan else {
        panic!("expected a web plan");
    };
    let html = root.join("synthetic-web/index.html");
    assert_eq!(url, &format!("file://{}", html.display()));
    (html, plan)
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

fn spawn_player(plan: &PlayPlan, players: &Players) -> Child {
    let mut busy = None;
    for _ in 0..50 {
        match launch(plan, players) {
            Ok(child) => return child,
            Err(LaunchError::Spawn { program, source })
                if source.kind() == ErrorKind::ExecutableFileBusy =>
            {
                busy = Some((program, source));
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("spawn player: {error}"),
        }
    }
    let (program, source) = busy.expect("busy spawn");
    panic!("player stayed busy: {}: {source}", program.display());
}

fn read_argv(record: &Path) -> Vec<String> {
    let bytes = fs::read(record).expect("read argv record");
    bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8(part.to_vec()).expect("utf-8 argv"))
        .collect()
}

fn web_players(
    root: &Path,
    plasmashell: PathBuf,
    web: PathBuf,
    muted: bool,
    web_plasma: bool,
) -> Players {
    let mut players = Players::default();
    assert!(
        players.web_plasma,
        "default web action is the Plasma wallpaper"
    );
    assert!(players.muted, "default web wallpaper is muted");
    players.plasmashell = plasmashell;
    players.web = web;
    players.plasma_data_home = root.join("data");
    players.muted = muted;
    players.web_plasma = web_plasma;
    players
}

fn assert_web_package(data_home: &Path) {
    let installed = plasma_web_wallpaper_dir(data_home);
    let metadata = fs::read_to_string(installed.join("metadata.json")).expect("installed metadata");
    assert!(metadata.contains(PLASMA_WEB_WALLPAPER_PLUGIN));
    assert!(metadata.contains("Plasma/Wallpaper"));
    assert!(
        !metadata.contains(PLASMA_VIDEO_WALLPAPER_PLUGIN),
        "web package names the video plugin"
    );
    let qml = fs::read_to_string(installed.join("contents/ui/main.qml")).expect("installed qml");
    assert!(
        qml.contains("import QtWebEngine"),
        "wallpaper does not use Qt WebEngine"
    );
    assert!(
        qml.contains("WebEngineView"),
        "wallpaper does not load the page in a web view"
    );
    assert!(
        qml.contains("root.configuration.PageUrl"),
        "wallpaper does not read the page URL"
    );
    assert!(
        qml.contains("audioMuted: root.configuration.Muted !== false"),
        "wallpaper does not mute when Muted is true: {qml}"
    );
    assert!(
        qml.contains("settings.localContentCanAccessFileUrls: true"),
        "wallpaper blocks local file access: {qml}"
    );
    assert!(
        qml.contains("settings.localContentCanAccessRemoteUrls: true"),
        "wallpaper blocks remote assets from the local page: {qml}"
    );
    let local_access = qml
        .find("settings.localContentCanAccessFileUrls: true")
        .expect("local access setting");
    let url_assign = qml.find("page.url = next").expect("page url assignment");
    assert!(
        local_access < url_assign,
        "page URL is assigned before local file access is enabled"
    );
    assert!(
        qml.contains("width: root.width"),
        "web view does not use the wallpaper width: {qml}"
    );
    assert!(
        qml.contains("height: root.height"),
        "web view does not use the wallpaper height: {qml}"
    );
    assert!(
        !qml.contains("url: root.configuration.PageUrl || \"\""),
        "empty PageUrl replaces the loaded page: {qml}"
    );
    let empty_guard = qml.find("if (next === \"\")").expect("empty url guard");
    assert!(
        empty_guard < url_assign,
        "empty page URL is applied before the guard"
    );
    assert!(
        !qml.contains("backgroundColor: \"black\""),
        "opaque black fill is the desktop when the page has not painted: {qml}"
    );
    assert!(
        qml.contains("backgroundColor: \"transparent\""),
        "web view does not let the page paint its own background: {qml}"
    );
    assert!(
        qml.contains("LifecycleState.Active"),
        "web view can be discarded and paint nothing: {qml}"
    );
    assert!(
        !qml.contains("about:blank"),
        "wallpaper navigates to a blank page: {qml}"
    );
    assert!(
        !qml.contains("xdg-open"),
        "wallpaper package names xdg-open"
    );
    assert!(
        !qml.contains("MediaPlayer"),
        "web package uses the video player"
    );
}

#[test]
fn web_and_video_packages_are_separate_directories() {
    let root = scratch_dir();
    let data = root.join("data");
    let video = install_plasma_video_wallpaper(&data).expect("install video");
    let web = install_plasma_web_wallpaper(&data).expect("install web");
    assert_eq!(video, plasma_wallpaper_dir(&data));
    assert_eq!(web, plasma_web_wallpaper_dir(&data));
    assert_ne!(video, web);

    let video_qml = fs::read_to_string(video.join("contents/ui/main.qml")).expect("video qml");
    assert!(video_qml.contains("MediaPlayer"));
    assert_web_package(&data);

    install_plasma_web_wallpaper(&data).expect("reinstall web");
    assert!(
        video.join("metadata.json").is_file(),
        "installing the web plugin deleted the video plugin"
    );
    let video_qml = fs::read_to_string(video.join("contents/ui/main.qml")).expect("video qml");
    assert!(video_qml.contains("MediaPlayer"));

    install_plasma_video_wallpaper(&data).expect("reinstall video");
    assert!(
        web.join("metadata.json").is_file(),
        "installing the video plugin deleted the web plugin"
    );
    assert_web_package(&data);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn default_web_launch_selects_the_plasma_plugin_and_does_not_spawn_xdg_open() {
    let root = scratch_dir();
    let (html, plan) = planned_web(&root);
    let PlayPlan::Web { ref url } = plan else {
        panic!("expected a web plan");
    };
    let script = plasma_web_wallpaper_script(url, true);

    assert_eq!(PLASMA_WEB_WALLPAPER_PLUGIN, "linux.wallpaper.web");
    assert!(
        script.contains(url),
        "script is missing the page URL: {script}"
    );
    assert!(
        script.contains(&html.display().to_string()),
        "script is missing the resolved page: {script}"
    );
    assert!(
        script.contains(PLASMA_WEB_WALLPAPER_PLUGIN),
        "script is missing the wallpaper plugin id: {script}"
    );
    assert!(
        !script.contains(PLASMA_VIDEO_WALLPAPER_PLUGIN),
        "web script names the video plugin: {script}"
    );
    assert!(
        script.contains("desktops()"),
        "script does not walk every desktop: {script}"
    );
    assert!(
        script.contains("writeConfig(\"PageUrl\","),
        "script does not write the page URL: {script}"
    );
    assert!(
        script.contains("writeConfig(\"Muted\", true)"),
        "default script is not muted: {script}"
    );
    assert!(
        !script.contains("xdg-open"),
        "script names xdg-open: {script}"
    );

    let video = install_plasma_video_wallpaper(&root.join("data")).expect("preinstall video");

    let plasma_record = root.join("plasma-argv");
    let open_record = root.join("xdg-argv");
    let plasmashell = write_argv_stub(&root, "qdbus6", &plasma_record);
    let xdg_open = write_argv_stub(&root, "xdg-open", &open_record);
    let players = web_players(&root, plasmashell.clone(), xdg_open, true, true);

    let mut child = spawn_player(&plan, &players);
    let status = child.wait().expect("wait for plasmashell tool");
    assert!(status.success());

    let argv = read_argv(&plasma_record);
    assert_eq!(
        argv,
        vec![
            plasmashell.display().to_string(),
            PLASMA_DBUS_SERVICE.to_string(),
            PLASMA_DBUS_PATH.to_string(),
            PLASMA_DBUS_METHOD.to_string(),
            script,
        ]
    );
    assert_eq!(PLASMA_DBUS_SERVICE, "org.kde.plasmashell");
    assert_eq!(PLASMA_DBUS_PATH, "/PlasmaShell");
    assert_eq!(PLASMA_DBUS_METHOD, "org.kde.PlasmaShell.evaluateScript");
    assert!(
        !open_record.exists(),
        "xdg-open was spawned for a Plasma web wallpaper"
    );

    assert_web_package(&players.plasma_data_home);
    assert!(
        video.join("metadata.json").is_file(),
        "launching the web wallpaper deleted the video plugin"
    );
    let video_qml = fs::read_to_string(video.join("contents/ui/main.qml")).expect("video qml");
    assert!(video_qml.contains("MediaPlayer"));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn web_wallpaper_script_can_leave_the_sound_on() {
    let root = scratch_dir();
    let (_, plan) = planned_web(&root);
    let PlayPlan::Web { ref url } = plan else {
        panic!("expected a web plan");
    };
    let script = plasma_web_wallpaper_script(url, false);
    assert!(script.contains(url));
    assert!(script.contains(PLASMA_WEB_WALLPAPER_PLUGIN));
    assert!(
        script.contains("writeConfig(\"Muted\", false)"),
        "sound-on script is still muted: {script}"
    );
    assert!(
        !script.contains("writeConfig(\"Muted\", true)"),
        "sound-on script also mutes: {script}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn missing_plasmashell_tool_is_an_error_and_does_not_spawn_xdg_open() {
    let root = scratch_dir();
    let (_, plan) = planned_web(&root);
    let open_record = root.join("xdg-argv");
    let xdg_open = write_argv_stub(&root, "xdg-open", &open_record);
    let missing = root.join("no-such-plasmashell");
    let players = web_players(&root, missing.clone(), xdg_open, true, true);

    let error = launch(&plan, &players).expect_err("missing plasmashell");
    assert!(
        error.to_string().contains("plasmashell tool not found"),
        "unclear plasmashell error: {error}"
    );
    assert!(
        error.to_string().contains(&missing.display().to_string()),
        "error does not name the missing tool: {error}"
    );
    assert!(
        !error.to_string().contains("xdg-open"),
        "missing plasmashell fell back to xdg-open wording: {error}"
    );
    match error {
        LaunchError::MissingPlasmashell { program, .. } => assert_eq!(program, missing),
        other => panic!("unexpected error: {other}"),
    }
    assert!(
        !open_record.exists(),
        "missing plasmashell spawned xdg-open"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn explicit_web_player_spawns_that_program_with_the_file_url_only() {
    let root = scratch_dir();
    let (_, plan) = planned_web(&root);
    let PlayPlan::Web { ref url } = plan else {
        panic!("expected a web plan");
    };
    let plasma_record = root.join("plasma-argv");
    let open_record = root.join("web-argv");
    let plasmashell = write_argv_stub(&root, "qdbus6", &plasma_record);
    let web = write_argv_stub(&root, "web-player", &open_record);
    let players = web_players(&root, plasmashell, web.clone(), true, false);

    let mut child = spawn_player(&plan, &players);
    let status = child.wait().expect("wait for web player");
    assert!(status.success());
    assert_eq!(
        read_argv(&open_record),
        vec![web.display().to_string(), url.clone()]
    );
    assert!(
        !plasma_record.exists(),
        "an explicit web player still called plasmashell"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn clearing_video_plasma_does_not_send_web_back_to_xdg_open() {
    let root = scratch_dir();
    let (_, plan) = planned_web(&root);
    let PlayPlan::Web { ref url } = plan else {
        panic!("expected a web plan");
    };
    let script = plasma_web_wallpaper_script(url, true);
    let plasma_record = root.join("plasma-argv");
    let open_record = root.join("xdg-argv");
    let video_record = root.join("video-argv");
    let plasmashell = write_argv_stub(&root, "qdbus6", &plasma_record);
    let xdg_open = write_argv_stub(&root, "xdg-open", &open_record);
    let video = write_argv_stub(&root, "mpv", &video_record);
    let mut players = web_players(&root, plasmashell.clone(), xdg_open, true, true);
    players.plasma = false;
    players.video = video;

    let mut child = spawn_player(&plan, &players);
    let status = child.wait().expect("wait for plasmashell tool");
    assert!(status.success());
    let argv = read_argv(&plasma_record);
    assert_eq!(argv[3], PLASMA_DBUS_METHOD);
    assert_eq!(argv.last().map(String::as_str), Some(script.as_str()));
    assert!(script.contains(PLASMA_WEB_WALLPAPER_PLUGIN));
    assert!(!open_record.exists(), "video override spawned xdg-open");
    assert!(
        !video_record.exists(),
        "video override spawned mpv for a web wallpaper"
    );
    let _ = fs::remove_dir_all(&root);
}

fn workshop_path(steam_root: &Path) -> PathBuf {
    steam_root
        .join("steamapps")
        .join("workshop")
        .join("content")
        .join("431960")
}

fn wallpaper_env(home: &Path, data_home: &Path, path: &Path, args: &[&str]) -> Output {
    let mut last = None;
    for _ in 0..50 {
        let output = Command::new(env!("CARGO_BIN_EXE_wallpaper"))
            .env("HOME", home)
            .env("XDG_DATA_HOME", data_home)
            .env("PATH", path)
            .args(args)
            .output()
            .expect("run wallpaper");
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("Text file busy") {
            return output;
        }
        last = Some(output);
        thread::sleep(Duration::from_millis(5));
    }
    last.expect("busy wallpaper")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "wallpaper failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn wallpaper_play_web_asks_plasmashell_and_does_not_spawn_xdg_open() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("steam-root");
    let html = write_web_project(&workshop_path(&steam_root).join("1004"));
    let data_home = scratch.join("data");
    let video_marker = data_home.join("plasma/wallpapers/linux.wallpaper.video/metadata.json");
    write_file(&video_marker, "{\"Id\":\"linux.wallpaper.video\"}\n");

    let bin = scratch.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let plasma_record = scratch.join("plasma-argv");
    let open_record = scratch.join("xdg-argv");
    let plasmashell = write_argv_stub(&bin, "qdbus6", &plasma_record);
    write_argv_stub(&bin, "xdg-open", &open_record);

    let steam_root = steam_root.display().to_string();
    let output = wallpaper_env(
        &scratch,
        &data_home,
        &bin,
        &["play", "1004", "--steam-root", &steam_root],
    );
    assert_success(&output);

    let argv = read_argv(&plasma_record);
    assert!(
        argv[0] == "qdbus6" || argv[0] == plasmashell.display().to_string(),
        "plasmashell was not qdbus6: {}",
        argv[0]
    );
    assert_eq!(argv[1], PLASMA_DBUS_SERVICE);
    assert_eq!(argv[2], PLASMA_DBUS_PATH);
    assert_eq!(argv[3], PLASMA_DBUS_METHOD);
    let script = argv.last().expect("script argument");
    assert!(script.contains(&html.display().to_string()), "{script}");
    assert!(
        script.contains(&format!("file://{}", html.display())),
        "{script}"
    );
    assert!(script.contains("linux.wallpaper.web"), "{script}");
    assert!(
        !script.contains("linux.wallpaper.video"),
        "web script selected the video plugin: {script}"
    );
    assert!(script.contains("writeConfig(\"Muted\", true)"), "{script}");
    assert!(!script.contains("xdg-open"), "{script}");
    assert!(!open_record.exists(), "wallpaper play spawned xdg-open");

    assert_web_package(&data_home);
    let marker = fs::read_to_string(&video_marker).expect("video plugin was deleted");
    assert!(marker.contains("linux.wallpaper.video"));
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn wallpaper_play_web_sound_unmutes_without_spawning_xdg_open() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("steam-root");
    write_web_project(&workshop_path(&steam_root).join("1004"));
    let data_home = scratch.join("data");
    let bin = scratch.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let plasma_record = scratch.join("plasma-argv");
    let open_record = scratch.join("xdg-argv");
    write_argv_stub(&bin, "qdbus6", &plasma_record);
    write_argv_stub(&bin, "xdg-open", &open_record);

    let steam_root = steam_root.display().to_string();
    let output = wallpaper_env(
        &scratch,
        &data_home,
        &bin,
        &["play", "1004", "--sound", "--steam-root", &steam_root],
    );
    assert_success(&output);
    let argv = read_argv(&plasma_record);
    let script = argv.last().expect("script argument");
    assert!(script.contains("linux.wallpaper.web"), "{script}");
    assert!(script.contains("writeConfig(\"Muted\", false)"), "{script}");
    assert!(!open_record.exists(), "sound flag spawned xdg-open");
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn wallpaper_play_web_without_plasmashell_does_not_spawn_xdg_open() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("steam-root");
    write_web_project(&workshop_path(&steam_root).join("1004"));
    let data_home = scratch.join("data");
    let bin = scratch.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let open_record = scratch.join("xdg-argv");
    write_argv_stub(&bin, "xdg-open", &open_record);

    let steam_root = steam_root.display().to_string();
    let output = wallpaper_env(
        &scratch,
        &data_home,
        &bin,
        &["play", "1004", "--steam-root", &steam_root],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "missing plasmashell exited 0: {stderr}"
    );
    assert!(
        stderr.contains("plasmashell tool not found"),
        "unclear error: {stderr}"
    );
    assert!(
        !stderr.contains("xdg-open"),
        "missing plasmashell fell back to xdg-open: {stderr}"
    );
    assert!(
        !open_record.exists(),
        "missing plasmashell spawned xdg-open"
    );
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn wallpaper_play_web_player_spawns_that_program_with_the_file_url() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("steam-root");
    let html = write_web_project(&workshop_path(&steam_root).join("1004"));
    let data_home = scratch.join("data");
    let bin = scratch.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let plasma_record = scratch.join("plasma-argv");
    let open_record = scratch.join("web-argv");
    write_argv_stub(&bin, "qdbus6", &plasma_record);
    let web = write_argv_stub(&bin, "web-player", &open_record);

    let steam_root = steam_root.display().to_string();
    let web_player = web.display().to_string();
    let output = wallpaper_env(
        &scratch,
        &data_home,
        &bin,
        &[
            "play",
            "1004",
            "--steam-root",
            &steam_root,
            "--web-player",
            &web_player,
        ],
    );
    assert_success(&output);
    assert_eq!(
        read_argv(&open_record),
        vec![
            web.display().to_string(),
            format!("file://{}", html.display()),
        ]
    );
    assert!(
        !plasma_record.exists(),
        "--web-player still called plasmashell"
    );
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn wallpaper_play_video_player_does_not_force_web_to_xdg_open() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("steam-root");
    let html = write_web_project(&workshop_path(&steam_root).join("1004"));
    let data_home = scratch.join("data");
    let bin = scratch.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let plasma_record = scratch.join("plasma-argv");
    let open_record = scratch.join("xdg-argv");
    let video_record = scratch.join("video-argv");
    let plasmashell = write_argv_stub(&bin, "qdbus6", &plasma_record);
    write_argv_stub(&bin, "xdg-open", &open_record);
    let video = write_argv_stub(&bin, "mpv", &video_record);

    let steam_root = steam_root.display().to_string();
    let video_player = video.display().to_string();
    let output = wallpaper_env(
        &scratch,
        &data_home,
        &bin,
        &[
            "play",
            "1004",
            "--steam-root",
            &steam_root,
            "--video-player",
            &video_player,
        ],
    );
    assert_success(&output);

    let argv = read_argv(&plasma_record);
    assert!(
        argv[0] == "qdbus6" || argv[0] == plasmashell.display().to_string(),
        "plasmashell was not qdbus6: {}",
        argv[0]
    );
    let script = argv.last().expect("script argument");
    assert!(script.contains("linux.wallpaper.web"), "{script}");
    assert!(
        script.contains(&format!("file://{}", html.display())),
        "{script}"
    );
    assert!(script.contains("writeConfig(\"Muted\", true)"), "{script}");
    assert!(!open_record.exists(), "--video-player spawned xdg-open");
    assert!(
        !video_record.exists(),
        "--video-player spawned mpv for a web wallpaper"
    );
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn sibling_assets_are_on_the_page_url_plasmashell_selects() {
    let root = scratch_dir();
    let project = root.join("synthetic-web");
    write_file(
        &project.join("project.json"),
        r#"{"type":"web","file":"index.html","title":"Synthetic Web"}"#,
    );
    write_file(
        &project.join("index.html"),
        "<!doctype html><script src=\"neighbor.js\"></script><img src=\"neighbor.png\" alt=\"\">",
    );
    write_file(&project.join("neighbor.js"), "/* synthetic */");
    write_file(&project.join("neighbor.png"), "synthetic image");

    let library = scan_library(&root).expect("scan");
    let plan = plan_playback(&library[0]).expect("web plan");
    let PlayPlan::Web { url } = plan else {
        panic!("expected a web plan");
    };
    let page = project.join("index.html");
    assert_eq!(url, format!("file://{}", page.display()));
    assert!(!url.is_empty(), "plasmashell would load an empty page");
    for name in ["neighbor.js", "neighbor.png"] {
        let (directory, _) = url.rsplit_once('/').expect("page url");
        let sibling = format!("{directory}/{name}");
        assert!(
            Path::new(sibling.trim_start_matches("file://")).is_file(),
            "{name} is not loadable from {url}"
        );
    }

    let script = plasma_web_wallpaper_script(&url, true);
    assert!(
        script.contains("desktop.wallpaperPlugin = \"linux.wallpaper.web\""),
        "evaluateScript does not select the web wallpaper: {script}"
    );
    assert!(
        script.contains(&format!("writeConfig(\"PageUrl\", \"{url}\")")),
        "evaluateScript does not write the page URL: {script}"
    );
    assert!(
        !script.contains("linux.wallpaper.video"),
        "web script selects the video plugin: {script}"
    );

    install_plasma_web_wallpaper(&root.join("data")).expect("install web");
    assert_web_package(&root.join("data"));
    let _ = fs::remove_dir_all(&root);
}
