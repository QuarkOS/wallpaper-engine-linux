use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use wallpaper_import::{scan_library, ImportError, Wallpaper};

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn scratch_dir() -> PathBuf {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("wallpaper-import-scan-{nanos}-{seq}"));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn write_project(dir: &std::path::Path, body: &str) {
    fs::create_dir_all(dir).expect("create project dir");
    fs::write(dir.join("project.json"), body).expect("write project.json");
}

#[test]
fn scans_fixture_root_in_sorted_path_order() {
    let root = fixture_root();
    let entries = scan_library(&root).expect("scan fixture root");

    assert_eq!(entries.len(), 3, "expected scene, video, and web");
    assert_eq!(entries[0].directory, root.join("scene"));
    assert_eq!(
        entries[0].wallpaper,
        Wallpaper::Unsupported {
            kind: "scene".into(),
        }
    );
    assert_eq!(entries[1].directory, root.join("video"));
    assert_eq!(
        entries[1].wallpaper,
        Wallpaper::Video {
            file: "wallpaper.mp4".into(),
            title: "Synthetic Video".into(),
        }
    );
    assert_eq!(entries[2].directory, root.join("web"));
    assert_eq!(
        entries[2].wallpaper,
        Wallpaper::Web {
            file: "index.html".into(),
            title: "Synthetic Web".into(),
        }
    );
}

#[test]
fn empty_root_returns_empty_list() {
    let root = scratch_dir();
    let entries = scan_library(&root).expect("scan empty root");
    assert!(entries.is_empty());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn child_without_project_json_is_skipped() {
    let root = scratch_dir();
    let video = root.join("video");
    write_project(
        &video,
        r#"{"type":"video","file":"wallpaper.mp4","title":"Kept"}"#,
    );
    fs::create_dir(root.join("no-project")).expect("create empty child");
    fs::write(root.join("notes.txt"), "not a project directory").expect("write stray file");

    let entries = scan_library(&root).expect("scan mixed root");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].directory, video);
    assert_eq!(
        entries[0].wallpaper,
        Wallpaper::Video {
            file: "wallpaper.mp4".into(),
            title: "Kept".into(),
        }
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn invalid_child_does_not_fail_the_scan() {
    let root = scratch_dir();
    write_project(
        &root.join("web"),
        r#"{"type":"web","file":"index.html","title":"Kept Web"}"#,
    );
    write_project(&root.join("broken"), "not json");
    write_project(
        &root.join("incomplete"),
        r#"{"type":"video","title":"Missing File"}"#,
    );

    let entries = scan_library(&root).expect("junk child is skipped");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].directory, root.join("web"));
    assert_eq!(
        entries[0].wallpaper,
        Wallpaper::Web {
            file: "index.html".into(),
            title: "Kept Web".into(),
        }
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn root_that_is_not_a_directory_errors() {
    let root = scratch_dir();
    let file = root.join("not-a-library");
    fs::write(&file, "nope").expect("write file");

    let error = scan_library(&file).expect_err("file path");
    assert!(
        matches!(error, ImportError::NotADirectory(ref path) if path == &file),
        "unexpected error: {error}"
    );

    let _ = fs::remove_dir_all(&root);
}
