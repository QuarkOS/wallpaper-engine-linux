use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wallpaper_import::workshop_dir;
use wallpaper_play::{play_first, LaunchError, PlayFirstError, Players};

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

fn scratch_dir() -> PathBuf {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("wallpaper-play-first-{nanos}-{seq}"));
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
    let mut file = File::create(&stub).expect("create stub");
    file.write_all(script.as_bytes()).expect("write stub");
    file.sync_all().expect("sync stub");
    let mut permissions = file.metadata().expect("stub metadata").permissions();
    permissions.set_mode(0o755);
    file.set_permissions(permissions).expect("chmod stub");
    drop(file);
    stub
}

/// Spawn through `play_first`. A just-written script can return ETXTBSY once.
fn spawn_first(library: &Path, players: &Players) -> Child {
    let mut busy = None;
    for _ in 0..50 {
        match play_first(library, players) {
            Ok(child) => return child,
            Err(PlayFirstError::Launch(LaunchError::Spawn { program, source }))
                if source.kind() == ErrorKind::ExecutableFileBusy =>
            {
                busy = Some((program, source));
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("play first: {error}"),
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

fn players_using(stub: PathBuf) -> Players {
    Players {
        video: stub.clone(),
        web: stub,
    }
}

#[test]
fn play_first_launches_the_first_video_and_skips_unsupported() {
    let steam_root = scratch_dir();
    let library = steam_root
        .join("steamapps")
        .join("workshop")
        .join("content")
        .join("431960");
    fs::create_dir_all(&library).expect("create workshop");

    write_project(
        &library.join("1001"),
        r#"{"type":"scene","file":"scene.json","title":"Synthetic Scene"}"#,
    );
    write_project(
        &library.join("1002"),
        r#"{"type":"application","file":"app.exe","title":"Synthetic App"}"#,
    );
    let video = library.join("1003");
    write_project(
        &video,
        r#"{"type":"video","file":"wallpaper.mp4","title":"Synthetic Video"}"#,
    );
    let media = video.join("wallpaper.mp4");
    write_file(&media, "synthetic video bytes");
    let web = library.join("1004");
    write_project(
        &web,
        r#"{"type":"web","file":"index.html","title":"Synthetic Web"}"#,
    );
    write_file(
        &web.join("index.html"),
        "<!doctype html><title>Synthetic Web</title>",
    );

    let library = workshop_dir(&steam_root).expect("workshop library");
    let record = steam_root.join("argv");
    let stub = write_argv_stub(&steam_root, &record);
    let mut child = spawn_first(&library, &players_using(stub.clone()));
    let status = child.wait().expect("wait for player");
    assert!(status.success());

    assert_eq!(
        read_argv(&record),
        vec![
            stub.display().to_string(),
            "--loop-file=inf".to_string(),
            media.display().to_string(),
        ]
    );
    let _ = fs::remove_dir_all(&steam_root);
}

#[test]
fn play_first_launches_the_first_web_entry() {
    let library = scratch_dir();
    write_project(
        &library.join("1001"),
        r#"{"type":"scene","file":"scene.json","title":"Synthetic Scene"}"#,
    );
    let web = library.join("1002");
    write_project(
        &web,
        r#"{"type":"web","file":"index.html","title":"Synthetic Web"}"#,
    );
    let html = web.join("index.html");
    write_file(&html, "<!doctype html><title>Synthetic Web</title>");
    let video = library.join("1003");
    write_project(
        &video,
        r#"{"type":"video","file":"wallpaper.mp4","title":"Synthetic Video"}"#,
    );
    write_file(&video.join("wallpaper.mp4"), "synthetic video bytes");

    let record = library.join("argv");
    let stub = write_argv_stub(&library, &record);
    let mut child = spawn_first(&library, &players_using(stub.clone()));
    let status = child.wait().expect("wait for player");
    assert!(status.success());

    assert_eq!(
        read_argv(&record),
        vec![
            stub.display().to_string(),
            format!("file://{}", html.display()),
        ]
    );
    let _ = fs::remove_dir_all(&library);
}

#[test]
fn only_scene_projects_error_and_launch_nothing() {
    let library = scratch_dir();
    write_project(
        &library.join("1001"),
        r#"{"type":"scene","file":"scene.json","title":"Synthetic Scene"}"#,
    );
    write_project(
        &library.join("1002"),
        r#"{"type":"scene","file":"other.json","title":"Another Scene"}"#,
    );

    let record = library.join("argv");
    let stub = write_argv_stub(&library, &record);
    let error = play_first(&library, &players_using(stub)).expect_err("only scenes");

    assert_eq!(error.to_string(), "library has no video or web wallpaper");
    assert!(
        matches!(error, PlayFirstError::NothingPlayable),
        "unexpected error: {error}"
    );
    assert!(
        !record.exists(),
        "scene library started a player: {record:?}"
    );
    let _ = fs::remove_dir_all(&library);
}
