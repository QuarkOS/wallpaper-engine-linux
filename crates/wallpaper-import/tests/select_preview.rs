use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use wallpaper_import::{load_wallpaper, select_preview, ImportError, Wallpaper};

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

fn scratch_dir() -> PathBuf {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("wallpaper-preview-{nanos}-{seq}"));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn write_project(dir: &Path, body: &str) {
    fs::create_dir_all(dir).expect("create project");
    fs::write(dir.join("project.json"), body).expect("write project.json");
}

fn write_file(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, body).expect("write file");
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
}

#[test]
fn image_preview_selects_the_file_inside_the_project() {
    let project = scratch_dir();
    write_project(
        &project,
        r#"{"type":"video","file":"wallpaper.mp4","title":"Synthetic Video","preview":"preview.png"}"#,
    );
    write_file(&project.join("preview.png"), "synthetic png bytes");

    let selected = select_preview(&project)
        .expect("preview")
        .expect("image preview");
    assert_eq!(selected.file, "preview.png");
    assert_eq!(selected.kind.as_str(), "image");
    assert_eq!(selected.path, project.join("preview.png"));

    let _ = fs::remove_dir_all(&project);
}

#[test]
fn video_preview_selects_a_nested_file() {
    let project = scratch_dir();
    write_project(
        &project,
        r#"{"type":"scene","file":"scene.json","title":"Synthetic Scene","preview":"clips/preview.mp4"}"#,
    );
    write_file(&project.join("clips/preview.mp4"), "synthetic video bytes");

    let selected = select_preview(&project)
        .expect("preview")
        .expect("video preview");
    assert_eq!(selected.file, "clips/preview.mp4");
    assert_eq!(selected.kind.as_str(), "video");
    assert_eq!(selected.path, project.join("clips/preview.mp4"));

    let _ = fs::remove_dir_all(&project);
}

#[test]
fn gif_preview_is_an_image() {
    let project = scratch_dir();
    write_project(
        &project,
        r#"{"type":"web","file":"index.html","title":"Synthetic Web","preview":"preview.gif"}"#,
    );
    write_file(&project.join("preview.gif"), "synthetic gif bytes");

    let selected = select_preview(&project)
        .expect("preview")
        .expect("gif preview");
    assert_eq!(selected.file, "preview.gif");
    assert_eq!(selected.kind.as_str(), "image");

    let _ = fs::remove_dir_all(&project);
}

#[test]
fn uppercase_extension_selects_an_image() {
    let project = scratch_dir();
    write_project(
        &project,
        r#"{"type":"video","file":"wallpaper.mp4","title":"Synthetic Video","preview":"preview.PNG"}"#,
    );
    write_file(&project.join("preview.PNG"), "synthetic png bytes");

    let selected = select_preview(&project)
        .expect("preview")
        .expect("uppercase preview");
    assert_eq!(selected.file, "preview.PNG");
    assert_eq!(selected.kind.as_str(), "image");

    let _ = fs::remove_dir_all(&project);
}

#[test]
fn named_preview_that_is_missing_selects_nothing() {
    let project = fixture("video");
    let selected = select_preview(&project).expect("fixture project");
    assert!(selected.is_none());
}

#[test]
fn absent_preview_selects_nothing() {
    let project = scratch_dir();
    write_project(
        &project,
        r#"{"type":"video","file":"wallpaper.mp4","title":"No Preview"}"#,
    );

    let selected = select_preview(&project).expect("project");
    assert!(selected.is_none());

    let _ = fs::remove_dir_all(&project);
}

#[test]
fn blank_preview_selects_nothing() {
    let project = scratch_dir();
    write_project(
        &project,
        r#"{"type":"video","file":"wallpaper.mp4","title":"Blank","preview":"   "}"#,
    );

    let selected = select_preview(&project).expect("project");
    assert!(selected.is_none());

    let _ = fs::remove_dir_all(&project);
}

#[test]
fn unknown_extension_selects_nothing() {
    let project = scratch_dir();
    write_project(
        &project,
        r#"{"type":"video","file":"wallpaper.mp4","title":"Notes","preview":"preview.txt"}"#,
    );
    write_file(&project.join("preview.txt"), "not a preview");

    let selected = select_preview(&project).expect("project");
    assert!(selected.is_none());

    let _ = fs::remove_dir_all(&project);
}

#[test]
fn preview_that_escapes_the_project_selects_nothing() {
    let parent = scratch_dir();
    let project = parent.join("project");
    write_project(
        &project,
        r#"{"type":"video","file":"wallpaper.mp4","title":"Escape","preview":"../preview.png"}"#,
    );
    write_file(&parent.join("preview.png"), "outside the project");

    let selected = select_preview(&project).expect("project");
    assert!(selected.is_none());

    let _ = fs::remove_dir_all(&parent);
}

#[test]
fn absolute_preview_selects_nothing() {
    let parent = scratch_dir();
    let project = parent.join("project");
    let outside = parent.join("preview.png");
    write_file(&outside, "absolute target");
    let preview = outside.display().to_string();
    write_project(
        &project,
        &format!(
            r#"{{"type":"video","file":"wallpaper.mp4","title":"Absolute","preview":"{preview}"}}"#
        ),
    );

    let selected = select_preview(&project).expect("project");
    assert!(selected.is_none());

    let _ = fs::remove_dir_all(&parent);
}

#[test]
fn symlink_preview_that_leaves_the_project_selects_nothing() {
    let parent = scratch_dir();
    let project = parent.join("project");
    write_project(
        &project,
        r#"{"type":"video","file":"wallpaper.mp4","title":"Link","preview":"preview.png"}"#,
    );
    let outside = parent.join("outside.png");
    write_file(&outside, "symlink target");
    symlink(&outside, project.join("preview.png")).expect("symlink");

    let selected = select_preview(&project).expect("project");
    assert!(selected.is_none());

    let _ = fs::remove_dir_all(&parent);
}

#[test]
fn non_string_preview_does_not_reject_the_project() {
    let project = scratch_dir();
    write_project(
        &project,
        r#"{"type":"video","file":"wallpaper.mp4","title":"Numeric Preview","preview":1}"#,
    );

    let wallpaper = load_wallpaper(&project).expect("video project");
    assert_eq!(
        wallpaper,
        Wallpaper::Video {
            file: "wallpaper.mp4".into(),
            title: "Numeric Preview".into(),
        }
    );
    let selected = select_preview(&project).expect("project");
    assert!(selected.is_none());

    let _ = fs::remove_dir_all(&project);
}

#[test]
fn missing_project_json_is_an_error() {
    let project = scratch_dir();
    let error = select_preview(&project).expect_err("empty directory");
    assert!(
        matches!(error, ImportError::MissingProjectJson(_)),
        "unexpected error: {error}"
    );
    let _ = fs::remove_dir_all(&project);
}
