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
    launch, plasma_wallpaper_dir, plasma_wallpaper_script, LaunchError, PlayError, PlayPlan,
    Players, PLASMA_DBUS_METHOD, PLASMA_DBUS_PATH, PLASMA_DBUS_SERVICE,
    PLASMA_VIDEO_WALLPAPER_PLUGIN,
};

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

fn scratch_dir() -> PathBuf {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("wallpaper-plasma-{nanos}-{seq}"));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
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

fn planned(root: &Path) -> Result<PlayPlan, PlayError> {
    let library = scan_library(root).expect("player scans the library");
    assert_eq!(library.len(), 1, "player sees one project");
    wallpaper_play::plan_playback(&library[0])
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

fn synthetic_video(root: &Path) -> (PathBuf, PlayPlan) {
    let project = root.join("synthetic-video");
    write_project(
        &project,
        r#"{"type":"video","file":"wallpaper.mp4","title":"Synthetic Video"}"#,
    );
    write_file(&project.join("wallpaper.mp4"), "synthetic video bytes");
    let plan = planned(root).expect("video plan");
    let PlayPlan::Video { ref file, loops } = plan else {
        panic!("expected a video plan");
    };
    assert!(loops);
    (file.clone(), plan)
}

fn plasma_players(root: &Path, plasmashell: PathBuf, video: PathBuf, muted: bool) -> Players {
    let mut players = Players::default();
    assert!(
        players.plasma,
        "default video action is the Plasma wallpaper"
    );
    assert!(players.muted, "default video wallpaper is muted");
    players.plasmashell = plasmashell;
    players.video = video;
    players.plasma_data_home = root.join("data");
    players.muted = muted;
    players
}

#[test]
fn plasma_wallpaper_request_contains_the_file_and_does_not_spawn_mpv() {
    let root = scratch_dir();
    let (file, plan) = synthetic_video(&root);
    let script = plasma_wallpaper_script(&file, true).expect("plasma script");
    let file_text = file.display().to_string();

    assert_eq!(PLASMA_VIDEO_WALLPAPER_PLUGIN, "linux.wallpaper.video");
    assert!(
        script.contains(&file_text),
        "script is missing the resolved file: {script}"
    );
    assert!(
        script.contains(PLASMA_VIDEO_WALLPAPER_PLUGIN),
        "script is missing the wallpaper plugin id: {script}"
    );
    assert!(
        script.contains("desktops()"),
        "script does not walk every desktop: {script}"
    );
    assert!(
        script.contains("writeConfig(\"Muted\", true)"),
        "default script is not muted: {script}"
    );
    assert!(!script.contains("mpv"), "script names mpv: {script}");
    assert!(
        !script.contains("--loop-file"),
        "script uses an mpv loop flag: {script}"
    );

    let plasma_record = root.join("plasma-argv");
    let mpv_record = root.join("mpv-argv");
    let plasmashell = write_argv_stub(&root, "qdbus6", &plasma_record);
    let mpv = write_argv_stub(&root, "mpv", &mpv_record);
    let players = plasma_players(&root, plasmashell.clone(), mpv, true);

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
        !mpv_record.exists(),
        "mpv was spawned for a Plasma wallpaper"
    );

    let installed = plasma_wallpaper_dir(&players.plasma_data_home);
    let metadata = fs::read_to_string(installed.join("metadata.json")).expect("installed metadata");
    assert!(metadata.contains(PLASMA_VIDEO_WALLPAPER_PLUGIN));
    assert!(metadata.contains("Plasma/Wallpaper"));
    let qml = fs::read_to_string(installed.join("contents/ui/main.qml")).expect("installed qml");
    assert!(
        qml.contains("MediaPlayer"),
        "wallpaper does not use Qt Multimedia"
    );
    assert!(
        qml.contains("MediaPlayer.Infinite"),
        "wallpaper does not loop"
    );
    assert!(!qml.contains("mpv"), "wallpaper package names mpv");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn plasma_wallpaper_script_can_leave_the_sound_on() {
    let root = scratch_dir();
    let (file, _) = synthetic_video(&root);
    let script = plasma_wallpaper_script(&file, false).expect("unmuted script");
    assert!(script.contains(&file.display().to_string()));
    assert!(script.contains(PLASMA_VIDEO_WALLPAPER_PLUGIN));
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
fn missing_plasmashell_tool_is_an_error_and_does_not_spawn_mpv() {
    let root = scratch_dir();
    let (_, plan) = synthetic_video(&root);
    let mpv_record = root.join("mpv-argv");
    let mpv = write_argv_stub(&root, "mpv", &mpv_record);
    let missing = root.join("no-such-plasmashell");
    let players = plasma_players(&root, missing.clone(), mpv, true);

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
        !error.to_string().contains("mpv"),
        "missing plasmashell fell back to mpv wording: {error}"
    );
    match error {
        LaunchError::MissingPlasmashell { program, .. } => assert_eq!(program, missing),
        other => panic!("unexpected error: {other}"),
    }
    assert!(!mpv_record.exists(), "missing plasmashell spawned mpv");
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

#[test]
fn wallpaper_play_asks_plasmashell_and_does_not_spawn_mpv() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("steam-root");
    let media = {
        let library = workshop_path(&steam_root);
        let project = library.join("1003");
        write_project(
            &project,
            r#"{"type":"video","file":"wallpaper.mp4","title":"Synthetic Video"}"#,
        );
        let media = project.join("wallpaper.mp4");
        write_file(&media, "synthetic video bytes");
        media
    };
    let data_home = scratch.join("data");
    let bin = scratch.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let plasma_record = scratch.join("plasma-argv");
    let mpv_record = scratch.join("mpv-argv");
    let plasmashell = write_argv_stub(&bin, "qdbus6", &plasma_record);
    let _mpv = write_argv_stub(&bin, "mpv", &mpv_record);

    let steam_root = steam_root.display().to_string();
    let output = wallpaper_env(
        &scratch,
        &data_home,
        &bin,
        &["play", "--steam-root", &steam_root],
    );
    assert!(
        output.status.success(),
        "wallpaper failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let argv = read_argv(&plasma_record);
    assert!(
        argv[0] == "qdbus6" || argv[0] == plasmashell.display().to_string(),
        "plasmashell was not qdbus6: {}",
        argv[0]
    );
    let script = argv.last().expect("script argument");
    assert!(script.contains(&media.display().to_string()), "{script}");
    assert!(script.contains("linux.wallpaper.video"), "{script}");
    assert!(script.contains("writeConfig(\"Muted\", true)"), "{script}");
    assert!(!script.contains("mpv"), "{script}");
    assert!(!mpv_record.exists(), "wallpaper play spawned mpv");

    let metadata =
        fs::read_to_string(data_home.join("plasma/wallpapers/linux.wallpaper.video/metadata.json"))
            .expect("plugin installed into the user Plasma wallpaper directory");
    assert!(metadata.contains("linux.wallpaper.video"));
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn wallpaper_play_sound_unmutes_without_spawning_mpv() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("steam-root");
    let library = workshop_path(&steam_root);
    let project = library.join("1003");
    write_project(
        &project,
        r#"{"type":"video","file":"wallpaper.mp4","title":"Synthetic Video"}"#,
    );
    write_file(&project.join("wallpaper.mp4"), "synthetic video bytes");

    let data_home = scratch.join("data");
    let bin = scratch.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let plasma_record = scratch.join("plasma-argv");
    let mpv_record = scratch.join("mpv-argv");
    write_argv_stub(&bin, "qdbus6", &plasma_record);
    write_argv_stub(&bin, "mpv", &mpv_record);

    let steam_root = steam_root.display().to_string();
    let output = wallpaper_env(
        &scratch,
        &data_home,
        &bin,
        &["play", "--sound", "--steam-root", &steam_root],
    );
    assert!(
        output.status.success(),
        "wallpaper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let argv = read_argv(&plasma_record);
    let script = argv.last().expect("script argument");
    assert!(script.contains("writeConfig(\"Muted\", false)"), "{script}");
    assert!(!mpv_record.exists(), "sound flag spawned mpv");
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn wallpaper_play_without_plasmashell_does_not_spawn_mpv() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("steam-root");
    let library = workshop_path(&steam_root);
    let project = library.join("1003");
    write_project(
        &project,
        r#"{"type":"video","file":"wallpaper.mp4","title":"Synthetic Video"}"#,
    );
    write_file(&project.join("wallpaper.mp4"), "synthetic video bytes");

    let data_home = scratch.join("data");
    let bin = scratch.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let mpv_record = scratch.join("mpv-argv");
    write_argv_stub(&bin, "mpv", &mpv_record);

    let steam_root = steam_root.display().to_string();
    let output = wallpaper_env(
        &scratch,
        &data_home,
        &bin,
        &["play", "--steam-root", &steam_root],
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
    assert!(!mpv_record.exists(), "missing plasmashell spawned mpv");
    let _ = fs::remove_dir_all(&scratch);
}
