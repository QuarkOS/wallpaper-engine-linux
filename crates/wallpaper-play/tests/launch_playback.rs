use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use wallpaper_import::scan_library;
use wallpaper_play::{launch, LaunchError, PlayError, PlayPlan, Players};

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

fn scratch_dir() -> PathBuf {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("wallpaper-launch-{nanos}-{seq}"));
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

/// A player scans the library, then asks for a plan for one entry.
fn planned(root: &Path) -> Result<PlayPlan, PlayError> {
    let library = scan_library(root).expect("player scans the library");
    assert_eq!(library.len(), 1, "player sees one project");
    wallpaper_play::plan_playback(&library[0])
}

/// Shell stub that writes its full argv, including argv[0], as NUL-separated
/// bytes and then exits.
fn write_argv_stub(dir: &Path, record: &Path) -> PathBuf {
    let record = record.display().to_string();
    assert!(
        !record.contains('\''),
        "record path must be single-quote safe"
    );
    let stub = dir.join("argv-stub");
    let script = format!("#!/bin/sh\nprintf '%s\\0' \"$0\" \"$@\" > '{record}'\n");
    fs::write(&stub, script).expect("write stub");
    let mut permissions = fs::metadata(&stub).expect("stub metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub, permissions).expect("chmod stub");
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

fn players_using(stub: PathBuf) -> Players {
    Players {
        video: stub.clone(),
        web: stub,
    }
}

#[test]
fn default_players_are_mpv_and_xdg_open() {
    let players = Players::default();
    assert_eq!(players.video, Path::new("mpv"));
    assert_eq!(players.web, Path::new("xdg-open"));
}

#[test]
fn launching_a_video_plan_records_the_file_and_an_infinite_loop() {
    let root = scratch_dir();
    let project = root.join("synthetic-video");
    write_project(
        &project,
        r#"{"type":"video","file":"wallpaper.mp4","title":"Synthetic Video"}"#,
    );
    write_file(&project.join("wallpaper.mp4"), "synthetic video bytes");

    let plan = planned(&root).expect("video plan");
    let PlayPlan::Video { ref file, loops } = plan else {
        panic!("expected a video plan");
    };
    assert!(loops);

    let record = root.join("argv");
    let stub = write_argv_stub(&root, &record);
    let mut child = launch(&plan, &players_using(stub.clone())).expect("spawn video player");
    let status = child.wait().expect("wait for video player");
    assert!(status.success());

    assert_eq!(
        read_argv(&record),
        vec![
            stub.display().to_string(),
            "--loop-file=inf".to_string(),
            file.display().to_string(),
        ]
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn launching_a_web_plan_records_the_file_url() {
    let root = scratch_dir();
    let project = root.join("synthetic-web");
    write_project(
        &project,
        r#"{"type":"web","file":"index.html","title":"Synthetic Web"}"#,
    );
    write_file(
        &project.join("index.html"),
        "<!doctype html><title>Synthetic Web</title>",
    );

    let plan = planned(&root).expect("web plan");
    let PlayPlan::Web { ref url } = plan else {
        panic!("expected a web plan");
    };

    let record = root.join("argv");
    let stub = write_argv_stub(&root, &record);
    let mut child = launch(&plan, &players_using(stub.clone())).expect("spawn web player");
    let status = child.wait().expect("wait for web player");
    assert!(status.success());

    assert_eq!(
        read_argv(&record),
        vec![stub.display().to_string(), url.clone()]
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn missing_player_executable_is_an_error() {
    let root = scratch_dir();
    let missing_video = root.join("no-such-video-player");
    let missing_web = root.join("no-such-web-player");
    let players = Players {
        video: missing_video.clone(),
        web: missing_web.clone(),
    };

    let video = PlayPlan::Video {
        file: root.join("wallpaper.mp4"),
        loops: true,
    };
    let error = launch(&video, &players).expect_err("missing video player");
    assert_eq!(
        error.to_string(),
        format!(
            "player executable not found: {}: No such file or directory (os error 2)",
            missing_video.display()
        )
    );
    match error {
        LaunchError::MissingPlayer { program, .. } => assert_eq!(program, missing_video),
        other => panic!("unexpected error: {other}"),
    }

    let web = PlayPlan::Web {
        url: "file:///tmp/synthetic/index.html".to_string(),
    };
    let error = launch(&web, &players).expect_err("missing web player");
    match error {
        LaunchError::MissingPlayer { program, .. } => assert_eq!(program, missing_web),
        other => panic!("unexpected error: {other}"),
    }
    let _ = fs::remove_dir_all(&root);
}
