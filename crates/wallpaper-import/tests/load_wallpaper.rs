use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use wallpaper_import::{load_wallpaper, ImportError, Wallpaper};

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
}

fn scratch_dir() -> PathBuf {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("wallpaper-import-{nanos}-{seq}"));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

#[test]
fn loads_video_project() {
    let wallpaper = load_wallpaper(fixture("video")).expect("video project");
    assert_eq!(
        wallpaper,
        Wallpaper::Video {
            file: "wallpaper.mp4".into(),
            title: "Synthetic Video".into(),
        }
    );
}

#[test]
fn loads_web_project() {
    let wallpaper = load_wallpaper(fixture("web")).expect("web project");
    assert_eq!(
        wallpaper,
        Wallpaper::Web {
            file: "index.html".into(),
            title: "Synthetic Web".into(),
        }
    );
}

#[test]
fn scene_project_is_unsupported() {
    let wallpaper = load_wallpaper(fixture("scene")).expect("scene project");
    assert_eq!(
        wallpaper,
        Wallpaper::Unsupported {
            kind: "scene".into(),
        }
    );
}

#[test]
fn application_project_is_unsupported() {
    let dir = scratch_dir();
    fs::write(
        dir.join("project.json"),
        r#"{"type":"application","file":"app.exe","title":"Synthetic App"}"#,
    )
    .expect("write project.json");

    let wallpaper = load_wallpaper(&dir).expect("application project");
    assert_eq!(
        wallpaper,
        Wallpaper::Unsupported {
            kind: "application".into(),
        }
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn missing_project_json_errors() {
    let dir = scratch_dir();
    let error = load_wallpaper(&dir).expect_err("empty directory");
    assert!(
        matches!(error, ImportError::MissingProjectJson(_)),
        "unexpected error: {error}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn video_without_file_errors() {
    let dir = scratch_dir();
    fs::write(
        dir.join("project.json"),
        r#"{"type":"video","title":"No File"}"#,
    )
    .expect("write project.json");

    let error = load_wallpaper(&dir).expect_err("video missing file");
    match error {
        ImportError::MissingField { field } => assert_eq!(field, "file"),
        other => panic!("unexpected error: {other}"),
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn path_that_is_not_a_directory_errors() {
    let dir = scratch_dir();
    let file = dir.join("not-a-project");
    fs::write(&file, "nope").expect("write file");

    let error = load_wallpaper(&file).expect_err("file path");
    assert!(
        matches!(error, ImportError::NotADirectory(ref path) if path == &file),
        "unexpected error: {error}"
    );

    let _ = fs::remove_dir_all(&dir);
}
