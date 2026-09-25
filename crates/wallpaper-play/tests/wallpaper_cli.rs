use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
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
    let dir = std::env::temp_dir().join(format!("wallpaper-cli-{nanos}-{seq}"));
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

/// `libraryfolders.vdf` whose quoted `path` entries are `libraries`, in order.
fn write_library_folders(steam_root: &Path, libraries: &[&Path]) {
    let mut body = String::from("\"libraryfolders\"\n{\n");
    for (index, library) in libraries.iter().enumerate() {
        body.push_str(&format!(
            "\t\"{index}\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t\t\"label\"\t\t\"\"\n\t\t\"apps\"\n\t\t{{\n\t\t\t\"431960\"\t\t\"0\"\n\t\t}}\n\t}}\n",
            library.display()
        ));
    }
    body.push_str("}\n");
    write_file(&steam_root.join("steamapps/libraryfolders.vdf"), &body);
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

/// Shell stub that writes its full argv, including argv[0], as NUL-separated
/// bytes and then exits.
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

/// Run `wallpaper`. A just-written stub can return ETXTBSY once; retry that.
fn wallpaper(home: &Path, args: &[&str]) -> Output {
    let mut last = None;
    for _ in 0..50 {
        let output = Command::new(env!("CARGO_BIN_EXE_wallpaper"))
            .env("HOME", home)
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
    let output = last.expect("busy wallpaper");
    panic!(
        "player stayed busy: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "wallpaper failed: status {:?}\nstdout {}\nstderr {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_failed_without_player(output: &Output, records: &[PathBuf]) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "wallpaper exited 0\nstdout {}\nstderr {stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!stderr.contains("panicked"), "wallpaper panicked: {stderr}");
    for record in records {
        assert!(!record.exists(), "player was spawned: {}", record.display());
    }
}

#[test]
fn play_with_steam_root_launches_the_first_video() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("steam-root");
    let library = workshop_path(&steam_root);
    write_scene(&library, "1001");
    write_application(&library, "1002");
    let media = write_video(&library, "1003", "wallpaper.mp4");
    write_web(&library, "1004", "index.html");

    let decoy_home = scratch.join("decoy-home");
    let decoy = workshop_path(&decoy_home.join(".steam/steam"));
    write_video(&decoy, "1001", "decoy.mp4");

    let stubs = stubs_in(&scratch);
    let steam_root = steam_root.display().to_string();
    let video_player = stubs.video.display().to_string();
    let web_player = stubs.web.display().to_string();
    let output = wallpaper(
        &decoy_home,
        &[
            "play",
            "--steam-root",
            &steam_root,
            "--video-player",
            &video_player,
            "--web-player",
            &web_player,
        ],
    );
    assert_success(&output);

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
        "web player was spawned for a video wallpaper"
    );
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn play_without_steam_root_uses_the_first_home_root_with_a_workshop() {
    let home = scratch_dir();
    fs::create_dir_all(home.join(".steam/steam/steamapps/common")).expect("first root");
    let second = workshop_path(&home.join(".local/share/Steam"));
    write_scene(&second, "1001");
    let html = write_web(&second, "1002", "index.html");
    write_video(&second, "1003", "later.mp4");
    let third = workshop_path(&home.join(".steam/root"));
    write_video(&third, "1001", "root-video.mp4");

    let stubs = stubs_in(&home);
    let video_player = stubs.video.display().to_string();
    let web_player = stubs.web.display().to_string();
    let output = wallpaper(
        &home,
        &[
            "play",
            "--video-player",
            &video_player,
            "--web-player",
            &web_player,
        ],
    );
    assert_success(&output);

    assert_eq!(
        read_argv(&stubs.web_record),
        vec![
            stubs.web.display().to_string(),
            format!("file://{}", html.display()),
        ]
    );
    assert!(
        !stubs.video_record.exists(),
        "video player was spawned for a web wallpaper"
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn play_without_steam_root_prefers_the_earliest_root() {
    let home = scratch_dir();
    let first = workshop_path(&home.join(".steam/steam"));
    write_scene(&first, "1001");
    let media = write_video(&first, "1002", "first.mp4");
    let second = workshop_path(&home.join(".local/share/Steam"));
    write_video(&second, "1001", "second.mp4");
    let third = workshop_path(&home.join(".steam/root"));
    write_video(&third, "1001", "third.mp4");

    let stubs = stubs_in(&home);
    let video_player = stubs.video.display().to_string();
    let web_player = stubs.web.display().to_string();
    let output = wallpaper(
        &home,
        &[
            "play",
            "--video-player",
            &video_player,
            "--web-player",
            &web_player,
        ],
    );
    assert_success(&output);

    assert_eq!(
        read_argv(&stubs.video_record),
        vec![
            stubs.video.display().to_string(),
            "--loop-file=inf".to_string(),
            media.display().to_string(),
        ]
    );
    assert!(!stubs.web_record.exists(), "web player was spawned");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn missing_home_roots_exit_nonzero_and_do_not_spawn() {
    let home = scratch_dir();
    let stubs = stubs_in(&home);
    let video_player = stubs.video.display().to_string();
    let web_player = stubs.web.display().to_string();
    let output = wallpaper(
        &home,
        &[
            "play",
            "--video-player",
            &video_player,
            "--web-player",
            &web_player,
        ],
    );

    assert_failed_without_player(&output, &[stubs.video_record, stubs.web_record]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(".steam/steam"),
        "stderr did not name the searched roots: {stderr}"
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn roots_without_a_workshop_directory_exit_nonzero_and_do_not_spawn() {
    let home = scratch_dir();
    fs::create_dir_all(home.join(".steam/steam")).expect("steam");
    fs::create_dir_all(home.join(".local/share/Steam")).expect("Steam");
    fs::create_dir_all(home.join(".steam/root")).expect("root");
    fs::create_dir_all(home.join(".local/share/Steam/steamapps/workshop/content/999999"))
        .expect("other app");

    let stubs = stubs_in(&home);
    let video_player = stubs.video.display().to_string();
    let web_player = stubs.web.display().to_string();
    let output = wallpaper(
        &home,
        &[
            "play",
            "--video-player",
            &video_player,
            "--web-player",
            &web_player,
        ],
    );

    assert_failed_without_player(&output, &[stubs.video_record, stubs.web_record]);
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn steam_root_without_a_workshop_does_not_fall_back_to_home() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("empty-steam");
    fs::create_dir_all(&steam_root).expect("empty steam root");

    let home = scratch.join("home");
    let library = workshop_path(&home.join(".steam/steam"));
    write_video(&library, "1001", "from-home.mp4");

    let stubs = stubs_in(&scratch);
    let steam_root = steam_root.display().to_string();
    let video_player = stubs.video.display().to_string();
    let web_player = stubs.web.display().to_string();
    let output = wallpaper(
        &home,
        &[
            "play",
            "--steam-root",
            &steam_root,
            "--video-player",
            &video_player,
            "--web-player",
            &web_player,
        ],
    );

    assert_failed_without_player(&output, &[stubs.video_record, stubs.web_record]);
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn second_library_in_vdf_is_played_when_the_root_has_no_workshop() {
    let home = scratch_dir();
    let steam_root = home.join(".steam/steam");
    fs::create_dir_all(steam_root.join("steamapps")).expect("steamapps");
    let second = home.join("second library");
    let library = workshop_path(&second);
    write_scene(&library, "1001");
    let media = write_video(&library, "1002", "wallpaper.mp4");
    write_library_folders(&steam_root, &[&steam_root, &second]);

    let decoy = workshop_path(&home.join(".local/share/Steam"));
    write_video(&decoy, "1001", "decoy.mp4");

    let stubs = stubs_in(&home);
    let video_player = stubs.video.display().to_string();
    let web_player = stubs.web.display().to_string();
    let output = wallpaper(
        &home,
        &[
            "play",
            "--video-player",
            &video_player,
            "--web-player",
            &web_player,
        ],
    );
    assert_success(&output);

    assert_eq!(
        read_argv(&stubs.video_record),
        vec![
            stubs.video.display().to_string(),
            "--loop-file=inf".to_string(),
            media.display().to_string(),
        ]
    );
    assert!(!stubs.web_record.exists(), "web player was spawned");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn root_workshop_is_used_before_a_library_named_in_vdf() {
    let home = scratch_dir();
    let steam_root = home.join(".steam/steam");
    let library = workshop_path(&steam_root);
    let media = write_video(&library, "1001", "from-root.mp4");
    let second = home.join("second library");
    write_video(&workshop_path(&second), "1001", "from-library.mp4");
    write_library_folders(&steam_root, &[&steam_root, &second]);

    let stubs = stubs_in(&home);
    let video_player = stubs.video.display().to_string();
    let web_player = stubs.web.display().to_string();
    let output = wallpaper(
        &home,
        &[
            "play",
            "--video-player",
            &video_player,
            "--web-player",
            &web_player,
        ],
    );
    assert_success(&output);

    assert_eq!(
        read_argv(&stubs.video_record),
        vec![
            stubs.video.display().to_string(),
            "--loop-file=inf".to_string(),
            media.display().to_string(),
        ]
    );
    assert!(!stubs.web_record.exists(), "web player was spawned");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn missing_library_path_is_skipped() {
    let home = scratch_dir();
    let steam_root = home.join(".steam/steam");
    fs::create_dir_all(steam_root.join("steamapps")).expect("steamapps");
    let missing = home.join("missing-library");
    let second = home.join("present-library");
    let media = write_video(&workshop_path(&second), "1001", "wallpaper.mp4");
    write_library_folders(&steam_root, &[&missing, &second]);

    let decoy = workshop_path(&home.join(".local/share/Steam"));
    write_video(&decoy, "1001", "decoy.mp4");

    let stubs = stubs_in(&home);
    let video_player = stubs.video.display().to_string();
    let web_player = stubs.web.display().to_string();
    let output = wallpaper(
        &home,
        &[
            "play",
            "--video-player",
            &video_player,
            "--web-player",
            &web_player,
        ],
    );
    assert_success(&output);

    assert_eq!(
        read_argv(&stubs.video_record),
        vec![
            stubs.video.display().to_string(),
            "--loop-file=inf".to_string(),
            media.display().to_string(),
        ]
    );
    assert!(!stubs.web_record.exists(), "web player was spawned");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn missing_vdf_still_plays_the_root_workshop() {
    let home = scratch_dir();
    let steam_root = home.join(".steam/steam");
    let media = write_video(&workshop_path(&steam_root), "1001", "from-root.mp4");
    assert!(
        !steam_root.join("steamapps/libraryfolders.vdf").exists(),
        "this test needs a missing libraryfolders.vdf"
    );

    let stubs = stubs_in(&home);
    let video_player = stubs.video.display().to_string();
    let web_player = stubs.web.display().to_string();
    let output = wallpaper(
        &home,
        &[
            "play",
            "--video-player",
            &video_player,
            "--web-player",
            &web_player,
        ],
    );
    assert_success(&output);

    assert_eq!(
        read_argv(&stubs.video_record),
        vec![
            stubs.video.display().to_string(),
            "--loop-file=inf".to_string(),
            media.display().to_string(),
        ]
    );
    assert!(!stubs.web_record.exists(), "web player was spawned");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn unreadable_vdf_still_plays_the_root_workshop() {
    let home = scratch_dir();
    let steam_root = home.join(".steam/steam");
    let media = write_video(&workshop_path(&steam_root), "1001", "from-root.mp4");
    let decoy = home.join("decoy-library");
    write_video(&workshop_path(&decoy), "1001", "from-decoy.mp4");
    write_library_folders(&steam_root, &[&decoy]);
    let vdf = steam_root.join("steamapps/libraryfolders.vdf");
    let mut permissions = fs::metadata(&vdf).expect("vdf metadata").permissions();
    permissions.set_mode(0o000);
    fs::set_permissions(&vdf, permissions).expect("chmod vdf");

    let stubs = stubs_in(&home);
    let video_player = stubs.video.display().to_string();
    let web_player = stubs.web.display().to_string();
    let output = wallpaper(
        &home,
        &[
            "play",
            "--video-player",
            &video_player,
            "--web-player",
            &web_player,
        ],
    );
    assert_success(&output);

    assert_eq!(
        read_argv(&stubs.video_record),
        vec![
            stubs.video.display().to_string(),
            "--loop-file=inf".to_string(),
            media.display().to_string(),
        ]
    );
    assert!(!stubs.web_record.exists(), "web player was spawned");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn steam_root_flag_plays_a_library_named_in_vdf() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("steam-root");
    fs::create_dir_all(steam_root.join("steamapps")).expect("steamapps");
    let second = scratch.join("second library");
    let media = write_video(&workshop_path(&second), "1001", "from-library.mp4");
    write_library_folders(&steam_root, &[&steam_root, &second]);

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
    let output = wallpaper(
        &home,
        &[
            "play",
            "--steam-root",
            &steam_root,
            "--video-player",
            &video_player,
            "--web-player",
            &web_player,
        ],
    );
    assert_success(&output);

    assert_eq!(
        read_argv(&stubs.video_record),
        vec![
            stubs.video.display().to_string(),
            "--loop-file=inf".to_string(),
            media.display().to_string(),
        ]
    );
    assert!(!stubs.web_record.exists(), "web player was spawned");
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn list_prints_each_project_sorted_by_id() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("steam-root");
    let library = workshop_path(&steam_root);
    write_project(
        &library.join("1002"),
        r#"{"type":"web","file":"index.html","title":"Clock Page"}"#,
    );
    write_file(
        &library.join("1002/index.html"),
        "<!doctype html><title>Clock Page</title>",
    );
    write_project(
        &library.join("1001"),
        r#"{"type":"scene","file":"scene.json","title":"Night Window"}"#,
    );
    write_project(
        &library.join("1003"),
        r#"{"type":"video","file":"rain.mp4","title":"Rain Loop"}"#,
    );
    write_file(&library.join("1003/rain.mp4"), "synthetic video bytes");

    let stubs = stubs_in(&scratch);
    let steam_root = steam_root.display().to_string();
    let output = wallpaper(&scratch, &["list", "--steam-root", &steam_root]);
    assert_success(&output);

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    assert_eq!(
        stdout,
        "\
1001 scene Night Window
1002 web Clock Page
1003 video Rain Loop
"
    );
    assert!(
        !stubs.video_record.exists(),
        "list spawned the video player"
    );
    assert!(!stubs.web_record.exists(), "list spawned the web player");
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn play_id_launches_that_video() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("steam-root");
    let library = workshop_path(&steam_root);
    write_scene(&library, "1001");
    write_video(&library, "1002", "first.mp4");
    let media = write_video(&library, "1003", "chosen.mp4");

    let stubs = stubs_in(&scratch);
    let steam_root = steam_root.display().to_string();
    let video_player = stubs.video.display().to_string();
    let web_player = stubs.web.display().to_string();
    let output = wallpaper(
        &scratch,
        &[
            "play",
            "1003",
            "--steam-root",
            &steam_root,
            "--video-player",
            &video_player,
            "--web-player",
            &web_player,
        ],
    );
    assert_success(&output);

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
        "web player was spawned for a video wallpaper"
    );
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn play_id_launches_that_web() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("steam-root");
    let library = workshop_path(&steam_root);
    write_video(&library, "1001", "earlier.mp4");
    let html = write_web(&library, "1002", "index.html");

    let stubs = stubs_in(&scratch);
    let steam_root = steam_root.display().to_string();
    let video_player = stubs.video.display().to_string();
    let web_player = stubs.web.display().to_string();
    let output = wallpaper(
        &scratch,
        &[
            "play",
            "1002",
            "--steam-root",
            &steam_root,
            "--video-player",
            &video_player,
            "--web-player",
            &web_player,
        ],
    );
    assert_success(&output);

    assert_eq!(
        read_argv(&stubs.web_record),
        vec![
            stubs.web.display().to_string(),
            format!("file://{}", html.display()),
        ]
    );
    assert!(
        !stubs.video_record.exists(),
        "video player was spawned for a web wallpaper"
    );
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn play_scene_id_exits_nonzero_and_does_not_spawn() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("steam-root");
    let library = workshop_path(&steam_root);
    write_scene(&library, "1001");
    write_video(&library, "1002", "wallpaper.mp4");

    let stubs = stubs_in(&scratch);
    let steam_root = steam_root.display().to_string();
    let video_player = stubs.video.display().to_string();
    let web_player = stubs.web.display().to_string();
    let output = wallpaper(
        &scratch,
        &[
            "play",
            "1001",
            "--steam-root",
            &steam_root,
            "--video-player",
            &video_player,
            "--web-player",
            &web_player,
        ],
    );

    assert_failed_without_player(&output, &[stubs.video_record, stubs.web_record]);
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn play_unknown_id_exits_nonzero_and_does_not_spawn() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("steam-root");
    let library = workshop_path(&steam_root);
    write_video(&library, "1001", "wallpaper.mp4");

    let stubs = stubs_in(&scratch);
    let steam_root = steam_root.display().to_string();
    let video_player = stubs.video.display().to_string();
    let web_player = stubs.web.display().to_string();
    let output = wallpaper(
        &scratch,
        &[
            "play",
            "9999",
            "--steam-root",
            &steam_root,
            "--video-player",
            &video_player,
            "--web-player",
            &web_player,
        ],
    );

    assert_failed_without_player(&output, &[stubs.video_record, stubs.web_record]);
    let _ = fs::remove_dir_all(&scratch);
}
