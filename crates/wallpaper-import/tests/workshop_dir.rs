use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use wallpaper_import::{workshop_dir, ImportError};

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

fn scratch_dir() -> PathBuf {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("wallpaper-workshop-{nanos}-{seq}"));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn workshop_path(steam_root: &std::path::Path) -> PathBuf {
    steam_root
        .join("steamapps")
        .join("workshop")
        .join("content")
        .join("431960")
}

#[test]
fn workshop_dir_returns_content_431960_when_it_exists() {
    let steam_root = scratch_dir();
    let expected = workshop_path(&steam_root);
    fs::create_dir_all(&expected).expect("create workshop");
    fs::create_dir_all(steam_root.join("steamapps/workshop/content/999999"))
        .expect("create other app");

    let found = workshop_dir(&steam_root).expect("workshop directory");

    assert_eq!(found, expected);
    let _ = fs::remove_dir_all(&steam_root);
}

#[test]
fn missing_workshop_directory_is_an_error() {
    let steam_root = scratch_dir();
    fs::create_dir_all(steam_root.join("steamapps/workshop/content/999999"))
        .expect("create other app");
    let expected = workshop_path(&steam_root);

    let error = workshop_dir(&steam_root).expect_err("missing workshop");

    assert_eq!(
        error.to_string(),
        format!("workshop library not found: {}", expected.display())
    );
    match error {
        ImportError::MissingWorkshop(path) => assert_eq!(path, expected),
        other => panic!("unexpected error: {other}"),
    }
    let _ = fs::remove_dir_all(&steam_root);
}

#[test]
fn workshop_path_that_is_a_file_is_an_error() {
    let steam_root = scratch_dir();
    let expected = workshop_path(&steam_root);
    fs::create_dir_all(expected.parent().expect("content parent")).expect("create content");
    fs::write(&expected, "not a workshop directory").expect("write file");

    let error = workshop_dir(&steam_root).expect_err("file is not a workshop");

    match error {
        ImportError::MissingWorkshop(path) => assert_eq!(path, expected),
        other => panic!("unexpected error: {other}"),
    }
    let _ = fs::remove_dir_all(&steam_root);
}
