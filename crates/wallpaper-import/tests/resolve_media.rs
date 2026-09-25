use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use wallpaper_import::{resolve_media, ImportError, LibraryEntry, Wallpaper};

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

fn scratch_dir() -> PathBuf {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("wallpaper-import-media-{nanos}-{seq}"));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn entry(directory: PathBuf, wallpaper: Wallpaper) -> LibraryEntry {
    LibraryEntry {
        directory,
        wallpaper,
    }
}

fn write_file(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, body).expect("write file");
}

#[test]
fn resolves_video_file_that_exists() {
    let project = scratch_dir();
    let media = project.join("wallpaper.mp4");
    write_file(&media, "synthetic video bytes");

    let found = resolve_media(&entry(
        project.clone(),
        Wallpaper::Video {
            file: "wallpaper.mp4".into(),
            title: "Synthetic Video".into(),
        },
    ))
    .expect("video media");

    assert_eq!(found, project.join("wallpaper.mp4"));
    let _ = fs::remove_dir_all(&project);
}

#[test]
fn resolves_web_file_that_exists() {
    let project = scratch_dir();
    let media = project.join("index.html");
    write_file(&media, "<!doctype html><title>Synthetic Web</title>");

    let found = resolve_media(&entry(
        project.clone(),
        Wallpaper::Web {
            file: "index.html".into(),
            title: "Synthetic Web".into(),
        },
    ))
    .expect("web media");

    assert_eq!(found, project.join("index.html"));
    let _ = fs::remove_dir_all(&project);
}

#[test]
fn resolves_nested_relative_file() {
    let project = scratch_dir();
    let media = project.join("clips").join("loop.mp4");
    write_file(&media, "synthetic nested video");

    let found = resolve_media(&entry(
        project.clone(),
        Wallpaper::Video {
            file: "clips/loop.mp4".into(),
            title: "Nested".into(),
        },
    ))
    .expect("nested media");

    assert_eq!(found, project.join("clips/loop.mp4"));
    let _ = fs::remove_dir_all(&project);
}

#[test]
fn relative_climb_that_stays_inside_resolves() {
    let project = scratch_dir();
    fs::create_dir(project.join("clips")).expect("create clips");
    write_file(&project.join("wallpaper.mp4"), "synthetic video bytes");

    let found = resolve_media(&entry(
        project.clone(),
        Wallpaper::Video {
            file: "clips/../wallpaper.mp4".into(),
            title: "Climb Inside".into(),
        },
    ))
    .expect("contained climb");

    assert_eq!(found, project.join("clips/../wallpaper.mp4"));
    let _ = fs::remove_dir_all(&project);
}

#[test]
fn missing_video_file_errors() {
    let project = scratch_dir();
    let error = resolve_media(&entry(
        project.clone(),
        Wallpaper::Video {
            file: "wallpaper.mp4".into(),
            title: "Missing".into(),
        },
    ))
    .expect_err("missing video");

    match error {
        ImportError::MissingMedia { path } => assert_eq!(path, project.join("wallpaper.mp4")),
        other => panic!("unexpected error: {other}"),
    }

    let _ = fs::remove_dir_all(&project);
}

#[test]
fn missing_web_file_errors() {
    let project = scratch_dir();
    let error = resolve_media(&entry(
        project.clone(),
        Wallpaper::Web {
            file: "index.html".into(),
            title: "Missing Web".into(),
        },
    ))
    .expect_err("missing web");

    match error {
        ImportError::MissingMedia { path } => assert_eq!(path, project.join("index.html")),
        other => panic!("unexpected error: {other}"),
    }

    let _ = fs::remove_dir_all(&project);
}

#[test]
fn unsupported_does_not_invent_a_path() {
    let sentinel = PathBuf::from("/no-such-wallpaper-project/sentinel");
    for kind in ["scene", "application"] {
        let error = resolve_media(&entry(
            sentinel.clone(),
            Wallpaper::Unsupported { kind: kind.into() },
        ))
        .expect_err("unsupported");

        let rendered = error.to_string();
        assert!(
            !rendered.contains("sentinel"),
            "invented a path for {kind}: {rendered}"
        );
        assert!(
            !rendered.contains("no-such-wallpaper-project"),
            "invented a path for {kind}: {rendered}"
        );
        match error {
            ImportError::UnsupportedMedia { kind: got } => assert_eq!(got, kind),
            other => panic!("unexpected error: {other}"),
        }
    }
}

#[test]
fn parent_escape_errors_even_when_the_target_exists() {
    let parent = scratch_dir();
    let project = parent.join("project");
    fs::create_dir(&project).expect("create project");
    write_file(&parent.join("secret.mp4"), "outside the project");

    let error = resolve_media(&entry(
        project,
        Wallpaper::Video {
            file: "../secret.mp4".into(),
            title: "Escape".into(),
        },
    ))
    .expect_err("parent escape");

    assert!(
        matches!(error, ImportError::PathEscapes { .. }),
        "unexpected error: {error}"
    );
    let _ = fs::remove_dir_all(&parent);
}

#[test]
fn deeper_parent_escape_errors() {
    let parent = scratch_dir();
    let project = parent.join("project");
    fs::create_dir(&project).expect("create project");
    write_file(&parent.join("secret.mp4"), "outside the project");

    let error = resolve_media(&entry(
        project,
        Wallpaper::Web {
            file: "nested/../../secret.mp4".into(),
            title: "Escape".into(),
        },
    ))
    .expect_err("deeper escape");

    assert!(
        matches!(error, ImportError::PathEscapes { .. }),
        "unexpected error: {error}"
    );
    let _ = fs::remove_dir_all(&parent);
}

#[test]
fn absolute_file_errors_even_when_the_target_exists() {
    let parent = scratch_dir();
    let project = parent.join("project");
    fs::create_dir(&project).expect("create project");
    let outside = parent.join("secret.mp4");
    write_file(&outside, "absolute target");

    let error = resolve_media(&entry(
        project,
        Wallpaper::Video {
            file: outside.to_string_lossy().into_owned(),
            title: "Absolute".into(),
        },
    ))
    .expect_err("absolute escape");

    match error {
        ImportError::PathEscapes { file, .. } => {
            assert_eq!(file, outside.to_string_lossy().as_ref());
        }
        other => panic!("unexpected error: {other}"),
    }
    let _ = fs::remove_dir_all(&parent);
}

#[test]
fn symlink_that_leaves_the_project_errors() {
    let parent = scratch_dir();
    let project = parent.join("project");
    fs::create_dir(&project).expect("create project");
    let outside = parent.join("secret.mp4");
    write_file(&outside, "symlink target");
    std::os::unix::fs::symlink(&outside, project.join("wallpaper.mp4")).expect("symlink");

    let error = resolve_media(&entry(
        project,
        Wallpaper::Video {
            file: "wallpaper.mp4".into(),
            title: "Symlink".into(),
        },
    ))
    .expect_err("symlink escape");

    assert!(
        matches!(error, ImportError::PathEscapes { .. }),
        "unexpected error: {error}"
    );
    let _ = fs::remove_dir_all(&parent);
}

#[test]
fn symlink_that_stays_inside_resolves() {
    let project = scratch_dir();
    write_file(&project.join("wallpaper.mp4"), "synthetic video bytes");
    std::os::unix::fs::symlink("wallpaper.mp4", project.join("link.mp4")).expect("symlink");

    let found = resolve_media(&entry(
        project.clone(),
        Wallpaper::Video {
            file: "link.mp4".into(),
            title: "Inside Link".into(),
        },
    ))
    .expect("inside symlink");

    assert_eq!(found, project.join("link.mp4"));
    let _ = fs::remove_dir_all(&project);
}
