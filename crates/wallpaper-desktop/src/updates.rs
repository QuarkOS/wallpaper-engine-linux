//! GitHub release checks for the public wallpaper-engine-linux repository.
//!
//! Channels, matched by git tag and the GitHub prerelease flag:
//! - `release`: newest release whose tag starts with `v` and a digit, and
//!   whose prerelease flag is false
//! - `preview`: newest prerelease whose tag starts with `preview-`
//! - `nightly`: newest prerelease whose tag starts with `nightly-`
//!
//! The suffix after that prefix has to start with a digit, so a tag such as
//! `vanilla` is not a release. Newest means the latest `published_at`. The
//! chosen tag is an update only when that suffix is a newer version than the
//! running build. The running build is `WALLPAPER_VERSION` when that variable
//! is set, otherwise the desktop crate version.
//!
//! A download is kept only after `sha256sums.txt` contains the hash of
//! `wallpaper-engine-linux-x86_64.tar.gz`. A writable directory beside the
//! running executable receives `wallpaper` and `wallpaper-desktop`. Otherwise
//! the verified archive is stored in the settings directory. This module does
//! not run a root install and does not call sudo.

use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::io::{self, Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::settings::UpdateChannel;

/// Public GitHub releases list for this project.
pub const GITHUB_RELEASES_URL: &str =
    "https://api.github.com/repos/QuarkOS/wallpaper-engine-linux/releases?per_page=100";

pub const ARCHIVE_NAME: &str = "wallpaper-engine-linux-x86_64.tar.gz";
pub const SUMS_NAME: &str = "sha256sums.txt";

const NETWORK_MESSAGE: &str = "Could not check for updates.";
const DOWNLOAD_MESSAGE: &str = "Could not download the update.";
const HASH_MESSAGE: &str = "The download did not match sha256sums.txt.";
const ARCHIVE_MESSAGE: &str = "The archive does not contain wallpaper and wallpaper-desktop.";
pub const NEXT_LAUNCH_MESSAGE: &str = "The next launch uses the new build.";

/// Crate version, or `WALLPAPER_VERSION` when that variable is non-empty.
pub fn running_version() -> String {
    match env::var("WALLPAPER_VERSION") {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// Lower-case hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// True when a GNU `sha256sum` line names `file_name` and matches `bytes`.
pub fn sha256sums_match(sums: &str, file_name: &str, bytes: &[u8]) -> bool {
    let digest = sha256_hex(bytes);
    sums.lines().any(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return false;
        }
        let mut parts = line.split_whitespace();
        let Some(hash) = parts.next() else {
            return false;
        };
        let Some(name) = parts.next() else {
            return false;
        };
        if parts.next().is_some() {
            return false;
        }
        let name = name.trim_start_matches('*');
        let base = Path::new(name)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(name);
        hash.eq_ignore_ascii_case(&digest) && (name == file_name || base == file_name)
    })
}

/// Gzip-compressed tar of `files`. Each path is stored as given.
pub fn pack_gzip_tar(files: &[(&str, &[u8])]) -> io::Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, bytes) in files {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o755);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        builder.append_data(&mut header, name, *bytes)?;
    }
    let tar_bytes = builder.into_inner()?;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&tar_bytes)?;
    encoder.finish()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateOffer {
    pub channel: UpdateChannel,
    pub tag: String,
    pub notes: String,
    pub archive_url: String,
    pub sums_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    Available(UpdateOffer),
    Current { tag: String },
    None,
    Failed(String),
}

impl CheckOutcome {
    pub fn state(&self) -> &'static str {
        match self {
            CheckOutcome::Available(_) => "available",
            CheckOutcome::Current { .. } => "current",
            CheckOutcome::None => "none",
            CheckOutcome::Failed(_) => "error",
        }
    }

    pub fn ok(&self) -> bool {
        !matches!(self, CheckOutcome::Failed(_))
    }
}

/// Pick the newest matching release in a GitHub releases JSON array.
pub fn check_channel(body: &[u8], channel: UpdateChannel, current: &str) -> CheckOutcome {
    let releases: Vec<GhRelease> = match serde_json::from_slice(body) {
        Ok(releases) => releases,
        Err(_) => {
            return CheckOutcome::Failed("Could not read the release list.".to_string());
        }
    };
    let mut best: Option<(usize, &GhRelease)> = None;
    for (index, release) in releases.iter().enumerate() {
        if !channel_match(release, channel) {
            continue;
        }
        let replace = match best {
            None => true,
            Some((best_index, best_release)) => {
                newer_publication(release, index, best_release, best_index)
            }
        };
        if replace {
            best = Some((index, release));
        }
    }
    let Some((_, release)) = best else {
        return CheckOutcome::None;
    };
    let Some(version) = version_suffix(&release.tag_name, channel) else {
        return CheckOutcome::None;
    };
    if !is_newer(version, current) {
        return CheckOutcome::Current {
            tag: release.tag_name.clone(),
        };
    }
    let Some(archive_url) = asset_url(&release.assets, ARCHIVE_NAME) else {
        return CheckOutcome::Failed(format!(
            "The newest {} release has no {ARCHIVE_NAME}.",
            channel.as_str()
        ));
    };
    let Some(sums_url) = asset_url(&release.assets, SUMS_NAME) else {
        return CheckOutcome::Failed(format!(
            "The newest {} release has no {SUMS_NAME}.",
            channel.as_str()
        ));
    };
    CheckOutcome::Available(UpdateOffer {
        channel,
        tag: release.tag_name.clone(),
        notes: short_note(&release.body),
        archive_url: archive_url.to_string(),
        sums_url: sums_url.to_string(),
    })
}

/// GET `url`. HTTP failures use the check message; callers that are downloading
/// replace it with [`download_failure`].
pub fn fetch_bytes(url: &str) -> Result<Vec<u8>, String> {
    let response = minreq::get(url)
        .with_header("User-Agent", "wallpaper-engine-linux")
        .with_header("Accept", "application/vnd.github+json")
        .with_timeout(20)
        .send()
        .map_err(|_| NETWORK_MESSAGE.to_string())?;
    if !(200..300).contains(&response.status_code) {
        return Err(NETWORK_MESSAGE.to_string());
    }
    Ok(response.as_bytes().to_vec())
}

pub fn download_failure() -> String {
    DOWNLOAD_MESSAGE.to_string()
}

pub fn hash_failure() -> String {
    HASH_MESSAGE.to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    Installed,
    Saved { path: PathBuf },
}

impl InstallOutcome {
    pub fn state(&self) -> &'static str {
        match self {
            InstallOutcome::Installed => "installed",
            InstallOutcome::Saved { .. } => "saved",
        }
    }

    pub fn message(&self) -> String {
        match self {
            InstallOutcome::Installed => NEXT_LAUNCH_MESSAGE.to_string(),
            InstallOutcome::Saved { path } => {
                format!("The verified archive is at {}.", path.display())
            }
        }
    }

    pub fn path(&self) -> Option<&Path> {
        match self {
            InstallOutcome::Installed => None,
            InstallOutcome::Saved { path } => Some(path),
        }
    }
}

/// Unpack beside `install_dir` when that directory is writable.
/// Otherwise keep the verified archive in `data_dir`.
pub fn install_archive(
    bytes: &[u8],
    install_dir: &Path,
    data_dir: &Path,
) -> Result<InstallOutcome, String> {
    if directory_is_writable(install_dir) {
        extract_binaries(bytes, install_dir)?;
        Ok(InstallOutcome::Installed)
    } else {
        fs::create_dir_all(data_dir).map_err(|error| error.to_string())?;
        let path = data_dir.join(ARCHIVE_NAME);
        let partial = data_dir.join(format!("{ARCHIVE_NAME}.partial"));
        fs::write(&partial, bytes).map_err(|error| error.to_string())?;
        if let Err(error) = fs::rename(&partial, &path) {
            let _ = fs::remove_file(&partial);
            return Err(error.to_string());
        }
        Ok(InstallOutcome::Saved { path })
    }
}

fn directory_is_writable(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let probe = dir.join(format!(
        ".wallpaper-update-probe-{}-{nanos}",
        std::process::id()
    ));
    match File::create(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn extract_binaries(bytes: &[u8], dest: &Path) -> Result<(), String> {
    let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    let entries = archive.entries().map_err(|_| ARCHIVE_MESSAGE.to_string())?;
    let mut found: HashMap<String, Vec<u8>> = HashMap::new();
    for entry in entries {
        let mut entry = entry.map_err(|_| ARCHIVE_MESSAGE.to_string())?;
        let path = entry
            .path()
            .map_err(|_| ARCHIVE_MESSAGE.to_string())?
            .into_owned();
        if path
            .components()
            .any(|component| component == Component::ParentDir)
        {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name != "wallpaper" && name != "wallpaper-desktop" {
            continue;
        }
        if found.contains_key(name) {
            continue;
        }
        let mut buf = Vec::new();
        entry
            .read_to_end(&mut buf)
            .map_err(|_| ARCHIVE_MESSAGE.to_string())?;
        found.insert(name.to_string(), buf);
    }
    if !found.contains_key("wallpaper") || !found.contains_key("wallpaper-desktop") {
        return Err(ARCHIVE_MESSAGE.to_string());
    }
    let names = ["wallpaper", "wallpaper-desktop"];
    let temps: Vec<PathBuf> = names
        .iter()
        .map(|name| dest.join(format!(".{name}.wallpaper-update")))
        .collect();
    for (name, temp) in names.iter().zip(temps.iter()) {
        if let Err(error) = write_executable(temp, &found[*name]) {
            for path in &temps {
                let _ = fs::remove_file(path);
            }
            return Err(error);
        }
    }
    for (name, temp) in names.iter().zip(temps.iter()) {
        if let Err(error) = fs::rename(temp, dest.join(name)) {
            for path in &temps {
                let _ = fs::remove_file(path);
            }
            return Err(error.to_string());
        }
    }
    Ok(())
}

fn write_executable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = File::create(path).map_err(|error| error.to_string())?;
    file.write_all(bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn channel_match(release: &GhRelease, channel: UpdateChannel) -> bool {
    if release.draft {
        return false;
    }
    let tag = release.tag_name.as_str();
    let prefixed = match channel {
        UpdateChannel::Release => !release.prerelease && tag.starts_with('v'),
        UpdateChannel::Preview => release.prerelease && tag.starts_with("preview-"),
        UpdateChannel::Nightly => release.prerelease && tag.starts_with("nightly-"),
    };
    prefixed && version_suffix(tag, channel).is_some()
}

fn version_suffix<'a>(tag: &'a str, channel: UpdateChannel) -> Option<&'a str> {
    let rest = match channel {
        UpdateChannel::Release => tag.strip_prefix('v')?,
        UpdateChannel::Preview => tag.strip_prefix("preview-")?,
        UpdateChannel::Nightly => tag.strip_prefix("nightly-")?,
    };
    if rest
        .as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_digit())
    {
        Some(rest)
    } else {
        None
    }
}

fn newer_publication(
    candidate: &GhRelease,
    index: usize,
    best: &GhRelease,
    best_index: usize,
) -> bool {
    match (&candidate.published_at, &best.published_at) {
        (Some(candidate_at), Some(best_at)) => {
            candidate_at > best_at || (candidate_at == best_at && index < best_index)
        }
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (None, None) => index < best_index,
    }
}

/// `candidate` is a newer dotted version than `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    cmp_versions(candidate, current) == std::cmp::Ordering::Greater
}

fn cmp_versions(left: &str, right: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (left_core, left_pre) = split_pre(left.trim());
    let (right_core, right_pre) = split_pre(right.trim());
    let left_nums = numeric_parts(left_core);
    let right_nums = numeric_parts(right_core);
    match (left_nums, right_nums) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(left_nums), Some(right_nums)) => {
            let len = left_nums.len().max(right_nums.len());
            for index in 0..len {
                let left = left_nums.get(index).copied().unwrap_or(0);
                let right = right_nums.get(index).copied().unwrap_or(0);
                match left.cmp(&right) {
                    Ordering::Equal => {}
                    other => return other,
                }
            }
            match (left_pre, right_pre) {
                (None, None) => Ordering::Equal,
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (Some(left), Some(right)) => cmp_pre(left, right),
            }
        }
    }
}

fn split_pre(version: &str) -> (&str, Option<&str>) {
    match version.split_once('-') {
        Some((core, pre)) if !pre.is_empty() => (core, Some(pre)),
        Some((core, _)) => (core, None),
        None => (version, None),
    }
}

fn numeric_parts(version: &str) -> Option<Vec<u64>> {
    if version.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for part in version.split('.') {
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        parts.push(part.parse::<u64>().ok()?);
    }
    Some(parts)
}

fn cmp_pre(left: &str, right: &str) -> std::cmp::Ordering {
    let left: Vec<&str> = left.split('.').filter(|part| !part.is_empty()).collect();
    let right: Vec<&str> = right.split('.').filter(|part| !part.is_empty()).collect();
    let shared = left.len().min(right.len());
    for index in 0..shared {
        let order = cmp_ident(left[index], right[index]);
        if order != std::cmp::Ordering::Equal {
            return order;
        }
    }
    left.len().cmp(&right.len())
}

fn cmp_ident(left: &str, right: &str) -> std::cmp::Ordering {
    match (left.parse::<u64>(), right.parse::<u64>()) {
        (Ok(left), Ok(right)) => left.cmp(&right),
        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
        (Err(_), Err(_)) => left.cmp(right),
    }
}

fn asset_url<'a>(assets: &'a [GhAsset], name: &str) -> Option<&'a str> {
    assets.iter().find_map(|asset| {
        if asset.name == name && !asset.browser_download_url.is_empty() {
            Some(asset.browser_download_url.as_str())
        } else {
            None
        }
    })
}

fn short_note(body: &str) -> String {
    let line = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    if line.is_empty() {
        return "A newer build is available.".to_string();
    }
    let mut chars = line.chars();
    let clipped: String = chars.by_ref().take(240).collect();
    if chars.next().is_some() {
        let mut shortened: String = clipped.chars().take(239).collect();
        shortened.push('…');
        shortened
    } else {
        clipped
    }
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    body: String,
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::UpdateChannel;

    fn release(
        tag: &str,
        prerelease: bool,
        published: &str,
        body: &str,
        with_assets: bool,
    ) -> String {
        let assets = if with_assets {
            format!(
                r#"[{{"name":"{ARCHIVE_NAME}","browser_download_url":"http://fixture/{ARCHIVE_NAME}"}},{{"name":"{SUMS_NAME}","browser_download_url":"http://fixture/{SUMS_NAME}"}}]"#
            )
        } else {
            "[]".to_string()
        };
        format!(
            r#"{{"tag_name":"{tag}","prerelease":{prerelease},"draft":false,"body":{body},"published_at":"{published}","assets":{assets}}}"#,
            body = serde_json::to_string(body).expect("note")
        )
    }

    fn list(items: &[&str]) -> Vec<u8> {
        format!("[{}]", items.join(",")).into_bytes()
    }

    #[test]
    fn channels_follow_the_tag_prefix_and_prerelease_flag() {
        let body = list(&[
            &release("vanilla", false, "2026-09-06T00:00:00Z", "nope", true),
            &release("v0.9.0", true, "2026-09-05T00:00:00Z", "pre", true),
            &release(
                "v0.2.0",
                false,
                "2026-09-01T00:00:00Z",
                "Stable playback.",
                true,
            ),
            &release(
                "v0.8.0",
                false,
                "2026-08-01T00:00:00Z",
                "older stable",
                true,
            ),
            &release(
                "preview-0.3.0",
                true,
                "2026-09-03T00:00:00Z",
                "Preview note.",
                true,
            ),
            &release(
                "preview-0.1.0",
                true,
                "2026-08-02T00:00:00Z",
                "old preview",
                true,
            ),
            &release(
                "preview-0.4.0",
                false,
                "2026-09-04T00:00:00Z",
                "not a prerelease",
                true,
            ),
            &release(
                "nightly-0.4.0",
                true,
                "2026-09-04T00:00:00Z",
                "Nightly note.",
                true,
            ),
            &release(
                "nightly-9.0.0",
                false,
                "2026-09-07T00:00:00Z",
                "not nightly",
                true,
            ),
            &release(
                "nightly-20260930",
                true,
                "2026-09-02T00:00:00Z",
                "older night",
                true,
            ),
        ]);

        match check_channel(&body, UpdateChannel::Release, "0.1.0") {
            CheckOutcome::Available(offer) => {
                assert_eq!(offer.tag, "v0.2.0");
                assert_eq!(offer.notes, "Stable playback.");
                assert!(offer.archive_url.ends_with(ARCHIVE_NAME));
                assert!(offer.sums_url.ends_with(SUMS_NAME));
            }
            other => panic!("expected the newest stable v tag, got {other:?}"),
        }

        match check_channel(&body, UpdateChannel::Preview, "0.1.0") {
            CheckOutcome::Available(offer) => {
                assert_eq!(offer.tag, "preview-0.3.0");
                assert_eq!(offer.notes, "Preview note.");
            }
            other => panic!("expected preview-0.3.0, got {other:?}"),
        }

        match check_channel(&body, UpdateChannel::Nightly, "0.1.0") {
            CheckOutcome::Available(offer) => {
                assert_eq!(offer.tag, "nightly-0.4.0");
                assert_eq!(offer.notes, "Nightly note.");
            }
            other => panic!("expected nightly-0.4.0, got {other:?}"),
        }
    }

    #[test]
    fn an_equal_or_older_tag_is_current_and_an_empty_channel_is_none() {
        let body = list(&[&release(
            "v0.1.0",
            false,
            "2026-09-01T00:00:00Z",
            "same",
            true,
        )]);
        assert!(matches!(
            check_channel(&body, UpdateChannel::Release, "0.1.0"),
            CheckOutcome::Current { tag } if tag == "v0.1.0"
        ));
        assert!(matches!(
            check_channel(&body, UpdateChannel::Release, "0.2.0"),
            CheckOutcome::Current { .. }
        ));
        assert!(matches!(
            check_channel(&body, UpdateChannel::Nightly, "0.1.0"),
            CheckOutcome::None
        ));
        assert!(matches!(
            check_channel(b"[]", UpdateChannel::Release, "0.1.0"),
            CheckOutcome::None
        ));
        assert!(matches!(
            check_channel(b"{}", UpdateChannel::Release, "0.1.0"),
            CheckOutcome::Failed(_)
        ));
    }

    #[test]
    fn version_order_compares_numbers_before_prerelease_words() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(is_newer("0.2.0", "0.2.0-rc.1"));
        assert!(is_newer("0.2.0-rc.2", "0.2.0-rc.1"));
        assert!(is_newer("1", "0.9.9"));
        assert!(is_newer("20260930", "0.1.0"));
        assert!(!is_newer("0.2.0-rc.1", "0.2.0"));
    }

    #[test]
    fn a_release_without_the_archive_is_a_visible_error() {
        let body = list(&[&release(
            "v0.3.0",
            false,
            "2026-09-01T00:00:00Z",
            "missing files",
            false,
        )]);
        match check_channel(&body, UpdateChannel::Release, "0.1.0") {
            CheckOutcome::Failed(message) => {
                assert!(message.contains(ARCHIVE_NAME), "{message}");
            }
            other => panic!("expected a missing asset error, got {other:?}"),
        }
    }

    #[test]
    fn notes_use_the_first_line_and_stay_short() {
        let long = "n".repeat(300);
        let body = list(&[&release(
            "v0.3.0",
            false,
            "2026-09-01T00:00:00Z",
            &format!("\n\n{long}\nsecond"),
            true,
        )]);
        match check_channel(&body, UpdateChannel::Release, "0.1.0") {
            CheckOutcome::Available(offer) => {
                assert_eq!(offer.notes.chars().count(), 240);
                assert!(offer.notes.ends_with('…'));
            }
            other => panic!("expected a note, got {other:?}"),
        }
        let empty = list(&[&release(
            "v0.3.0",
            false,
            "2026-09-01T00:00:00Z",
            "  \n  ",
            true,
        )]);
        match check_channel(&empty, UpdateChannel::Release, "0.1.0") {
            CheckOutcome::Available(offer) => {
                assert_eq!(offer.notes, "A newer build is available.");
            }
            other => panic!("expected the fallback note, got {other:?}"),
        }
    }

    #[test]
    fn sha256sums_require_the_archive_name_and_hash() {
        let bytes = b"synthetic archive";
        let digest = sha256_hex(bytes);
        let sums = format!("{digest}  {ARCHIVE_NAME}\n{digest}  other.bin\n");
        assert!(sha256sums_match(&sums, ARCHIVE_NAME, bytes));
        assert!(sha256sums_match(
            &format!("{digest} *{ARCHIVE_NAME}\n"),
            ARCHIVE_NAME,
            bytes
        ));
        assert!(!sha256sums_match(
            &format!("{digest}\n"),
            ARCHIVE_NAME,
            bytes
        ));
        assert!(!sha256sums_match(
            &format!("deadbeef  {ARCHIVE_NAME}\n"),
            ARCHIVE_NAME,
            bytes
        ));
        assert!(!sha256sums_match(
            &format!("{digest}  other.bin\n"),
            ARCHIVE_NAME,
            bytes
        ));
    }

    #[test]
    fn a_writable_directory_receives_both_binaries() {
        let root = std::env::temp_dir().join(format!(
            "wallpaper-update-write-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let install = root.join("install");
        let data = root.join("data");
        fs::create_dir_all(&install).expect("install");
        let archive = pack_gzip_tar(&[
            ("dist/wallpaper", b"new-wallpaper"),
            ("dist/wallpaper-desktop", b"new-desktop"),
            ("README", b"skip me"),
        ])
        .expect("pack");
        let outcome = install_archive(&archive, &install, &data).expect("install");
        assert_eq!(outcome, InstallOutcome::Installed);
        assert_eq!(outcome.message(), NEXT_LAUNCH_MESSAGE);
        assert_eq!(
            fs::read(install.join("wallpaper")).expect("wallpaper"),
            b"new-wallpaper"
        );
        assert_eq!(
            fs::read(install.join("wallpaper-desktop")).expect("desktop"),
            b"new-desktop"
        );
        assert!(!install.join("README").exists());
        assert!(!data.join(ARCHIVE_NAME).exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_read_only_directory_keeps_the_verified_archive_in_the_data_dir() {
        let root = std::env::temp_dir().join(format!(
            "wallpaper-update-ro-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let install = root.join("install");
        let data = root.join("data");
        fs::create_dir_all(&install).expect("install");
        let mut permissions = fs::metadata(&install).expect("meta").permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(0o555);
            fs::set_permissions(&install, permissions).expect("chmod");
        }
        let archive = pack_gzip_tar(&[
            ("wallpaper", b"new-wallpaper"),
            ("wallpaper-desktop", b"new-desktop"),
        ])
        .expect("pack");
        let outcome = install_archive(&archive, &install, &data).expect("save");
        let InstallOutcome::Saved { path } = &outcome else {
            panic!("expected the archive to be saved, got {outcome:?}");
        };
        assert_eq!(path, &data.join(ARCHIVE_NAME));
        assert!(outcome.message().contains(&path.display().to_string()));
        assert_eq!(fs::read(path).expect("archive"), archive);
        assert!(!data.join(format!("{ARCHIVE_NAME}.partial")).exists());
        assert!(!install.join("wallpaper").exists());
        assert!(!install.join("wallpaper-desktop").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&install).expect("meta").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&install, permissions).expect("restore");
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_bad_archive_is_not_unpacked() {
        let root = std::env::temp_dir().join(format!(
            "wallpaper-update-bad-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let install = root.join("install");
        fs::create_dir_all(&install).expect("install");
        let archive = pack_gzip_tar(&[("wallpaper", b"only one")]).expect("pack");
        let error =
            install_archive(&archive, &install, &root.join("data")).expect_err("missing desktop");
        assert!(error.contains("wallpaper-desktop"), "{error}");
        assert!(!install.join("wallpaper").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn github_url_points_at_the_public_repository() {
        assert!(GITHUB_RELEASES_URL.contains("QuarkOS/wallpaper-engine-linux"));
        assert!(GITHUB_RELEASES_URL.contains("api.github.com"));
        assert!(GITHUB_RELEASES_URL.contains("/releases"));
    }
}
