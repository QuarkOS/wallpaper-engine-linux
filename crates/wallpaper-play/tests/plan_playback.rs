use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use wallpaper_import::{scan_library, LibraryEntry, Wallpaper};
use wallpaper_play::{plan_playback, PlayError, PlayPlan};

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

fn scratch_dir() -> PathBuf {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("wallpaper-play-{nanos}-{seq}"));
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
    plan_playback(&library[0])
}

#[test]
fn video_plan_names_the_resolved_file_and_loops() {
    let root = scratch_dir();
    let project = root.join("synthetic-video");
    write_project(
        &project,
        r#"{"type":"video","file":"wallpaper.mp4","title":"Synthetic Video"}"#,
    );
    write_file(&project.join("wallpaper.mp4"), "synthetic video bytes");

    let plan = planned(&root).expect("video plan");

    assert_eq!(
        plan,
        PlayPlan::Video {
            file: project.join("wallpaper.mp4"),
            loops: true,
        }
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn video_plan_names_a_nested_resolved_file() {
    let root = scratch_dir();
    let project = root.join("nested-video");
    write_project(
        &project,
        r#"{"type":"video","file":"clips/loop.mp4","title":"Nested"}"#,
    );
    write_file(&project.join("clips/loop.mp4"), "synthetic nested video");

    let plan = planned(&root).expect("nested video plan");

    assert_eq!(
        plan,
        PlayPlan::Video {
            file: project.join("clips/loop.mp4"),
            loops: true,
        }
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn web_plan_is_a_file_url_to_the_resolved_html() {
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
    let resolved = project.join("index.html");

    assert_eq!(
        plan,
        PlayPlan::Web {
            url: format!("file://{}", resolved.display()),
        }
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn web_plan_percent_encodes_spaces_and_hashes_in_the_file_url() {
    let root = scratch_dir();
    let project = root.join("my wallpapers");
    write_project(
        &project,
        r#"{"type":"web","file":"page #1.html","title":"Synthetic Web"}"#,
    );
    write_file(
        &project.join("page #1.html"),
        "<!doctype html><title>Synthetic Web</title>",
    );

    let plan = planned(&root).expect("web plan");

    assert_eq!(
        plan,
        PlayPlan::Web {
            url: format!("file://{}/my%20wallpapers/page%20%231.html", root.display()),
        }
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn unsupported_wallpapers_error() {
    let root = scratch_dir();
    for (kind, body) in [
        (
            "scene",
            r#"{"type":"scene","file":"scene.json","title":"Synthetic Scene"}"#,
        ),
        (
            "application",
            r#"{"type":"application","file":"app.exe","title":"Synthetic App"}"#,
        ),
    ] {
        let project = root.join(kind);
        write_project(&project, body);
        write_file(&project.join("ignored.txt"), "not played");

        let library = scan_library(&root).expect("scan");
        let entry = library
            .iter()
            .find(|entry| entry.directory == project)
            .expect("entry");
        let error = plan_playback(entry).expect_err(kind);

        assert_eq!(
            error.to_string(),
            format!("wallpaper type `{kind}` cannot be played")
        );
        match error {
            PlayError::Unsupported { kind: got } => assert_eq!(got, kind),
            other => panic!("unexpected error: {other}"),
        }
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn unsupported_does_not_invent_a_file_url() {
    let sentinel = PathBuf::from("/no-such-wallpaper-project/sentinel");
    let error = plan_playback(&LibraryEntry {
        directory: sentinel,
        wallpaper: Wallpaper::Unsupported {
            kind: "scene".into(),
        },
    })
    .expect_err("unsupported");

    let rendered = error.to_string();
    assert!(
        !rendered.contains("sentinel"),
        "invented a path: {rendered}"
    );
    assert!(
        !rendered.contains("no-such-wallpaper-project"),
        "invented a path: {rendered}"
    );
    match error {
        PlayError::Unsupported { kind } => assert_eq!(kind, "scene"),
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn missing_video_file_errors() {
    let root = scratch_dir();
    let project = root.join("missing-video");
    write_project(
        &project,
        r#"{"type":"video","file":"wallpaper.mp4","title":"Missing"}"#,
    );

    let error = planned(&root).expect_err("missing video");
    let expected = project.join("wallpaper.mp4");

    assert_eq!(
        error.to_string(),
        format!("media file not found: {}", expected.display())
    );
    match error {
        PlayError::MissingFile { path } => assert_eq!(path, expected),
        other => panic!("unexpected error: {other}"),
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn missing_web_file_errors() {
    let root = scratch_dir();
    let project = root.join("missing-web");
    write_project(
        &project,
        r#"{"type":"web","file":"index.html","title":"Missing Web"}"#,
    );

    let error = planned(&root).expect_err("missing web");
    let expected = project.join("index.html");

    assert_eq!(
        error.to_string(),
        format!("media file not found: {}", expected.display())
    );
    match error {
        PlayError::MissingFile { path } => assert_eq!(path, expected),
        other => panic!("unexpected error: {other}"),
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn escaped_path_is_not_a_plan() {
    let parent = scratch_dir();
    let project = parent.join("project");
    write_project(
        &project,
        r#"{"type":"video","file":"../secret.mp4","title":"Escape"}"#,
    );
    write_file(&parent.join("secret.mp4"), "outside the project");

    let error = planned(&parent).expect_err("escape");

    match error {
        PlayError::PathEscapes { file, .. } => assert_eq!(file, "../secret.mp4"),
        other => panic!("unexpected error: {other}"),
    }
    let _ = fs::remove_dir_all(&parent);
}
