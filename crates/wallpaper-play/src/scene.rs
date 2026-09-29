//! Visible image layers from a Wallpaper Engine scene project.
//!
//! Scene projects stay [`Wallpaper::Unsupported`](wallpaper_import::Wallpaper)
//! in the import crate. This module reads the scene JSON named by
//! `project.json`'s `file` field (usually `scene.json`) and keeps every
//! visible `ImageLayer` that resolves to a picture or a video inside the
//! project directory.
//!
//! An image path may be that picture or video, or a model JSON whose
//! `material` string points at a material JSON. The material carries a
//! relative texture path ending in png, jpg, jpeg, gif, webp, mp4, webm, or
//! mkv. Paths use the same containment rules as
//! [`resolve_media`](wallpaper_import::resolve_media): relative, no climb out
//! of the project, and a canonical target that stays inside the project.
//!
//! `visible: false` is skipped. Every other classname is skipped, including
//! ParticleSystem, TextLayer, Model, and Sound. An object with a script is
//! skipped. `scene.pkg` is never opened.

use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use super::{file_url, SceneLayer, SceneVisual};

/// Image and video layers in scene order.
///
/// An empty list means the scene cannot be played: the scene file is missing,
/// nothing resolved, or every candidate was skipped.
pub(crate) fn scene_layers(directory: &Path) -> Vec<SceneLayer> {
    let Some(scene) = read_scene_document(directory) else {
        return Vec::new();
    };
    let Some(objects) = scene.get("objects").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut layers = Vec::new();
    for object in objects {
        if !is_playable_image_layer(object) {
            continue;
        }
        let Some(image) = object.get("image").and_then(Value::as_str) else {
            continue;
        };
        if let Some(layer) = resolve_image_reference(directory, image) {
            layers.push(layer);
        }
    }
    layers
}

fn read_scene_document(directory: &Path) -> Option<Value> {
    let relative = scene_json_relative(directory)?;
    if is_scene_pkg(relative.as_str()) {
        return None;
    }
    let path = resolve_contained(directory, &relative)?;
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// `project.json` `file` when it names a JSON document.
///
/// `scene.pkg` is not opened. A package that names `scene.pkg` uses
/// `scene.json` beside it when that file exists. A missing `file` also uses
/// `scene.json`. Any other JSON name is used as written, including one that
/// fails containment, so a path that climbs out is not replaced with a
/// fallback.
fn scene_json_relative(directory: &Path) -> Option<String> {
    let Some(file) = project_file_field(directory) else {
        return Some("scene.json".to_string());
    };
    if is_scene_pkg(&file) {
        return Some("scene.json".to_string());
    }
    if file.to_ascii_lowercase().ends_with(".json") {
        return Some(file);
    }
    Some("scene.json".to_string())
}

fn project_file_field(directory: &Path) -> Option<String> {
    let text = fs::read_to_string(directory.join("project.json")).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let file = value.get("file").and_then(Value::as_str)?.trim();
    if file.is_empty() {
        None
    } else {
        Some(file.to_string())
    }
}

fn is_playable_image_layer(object: &Value) -> bool {
    if object.get("classname").and_then(Value::as_str) != Some("ImageLayer") {
        return false;
    }
    if matches!(object.get("visible"), Some(Value::Bool(false))) {
        return false;
    }
    !has_script(object)
}

fn has_script(object: &Value) -> bool {
    script_present(object.get("script")) || script_present(object.get("scripts"))
}

fn script_present(value: Option<&Value>) -> bool {
    match value {
        Some(Value::String(text)) => !text.trim().is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(map)) => !map.is_empty(),
        _ => false,
    }
}

fn resolve_image_reference(directory: &Path, image: &str) -> Option<SceneLayer> {
    let path = resolve_contained(directory, image)?;
    if let Some(visual) = visual_kind(&path) {
        return layer_from_path(&path, visual);
    }
    if !is_json_file(&path) {
        return None;
    }
    let model: Value = serde_json::from_str(&fs::read_to_string(&path).ok()?).ok()?;
    let material = model.get("material").and_then(Value::as_str)?.trim();
    if material.is_empty() {
        return None;
    }
    let material_path = resolve_contained(directory, material)?;
    let material: Value = serde_json::from_str(&fs::read_to_string(&material_path).ok()?).ok()?;
    resolve_material_texture(directory, &material)
}

fn resolve_material_texture(directory: &Path, material: &Value) -> Option<SceneLayer> {
    let mut candidates = texture_paths_in_passes(material);
    let mut extras = Vec::new();
    collect_visual_strings(material, &mut extras);
    for path in extras {
        if !candidates.iter().any(|existing| existing == &path) {
            candidates.push(path);
        }
    }
    for texture in candidates {
        if let Some(layer) = resolve_visual_file(directory, &texture) {
            return Some(layer);
        }
    }
    None
}

fn texture_paths_in_passes(material: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    let Some(passes) = material.get("passes").and_then(Value::as_array) else {
        return paths;
    };
    for pass in passes {
        match pass.get("textures") {
            Some(Value::String(text)) => push_visual(text, &mut paths),
            Some(Value::Array(items)) => {
                for item in items {
                    if let Some(text) = item.as_str() {
                        push_visual(text, &mut paths);
                    }
                }
            }
            _ => {}
        }
    }
    paths
}

fn collect_visual_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => push_visual(text, out),
        Value::Array(items) => {
            for item in items {
                collect_visual_strings(item, out);
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                collect_visual_strings(item, out);
            }
        }
        _ => {}
    }
}

fn push_visual(text: &str, out: &mut Vec<String>) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    if visual_kind(Path::new(text)).is_some() {
        out.push(text.to_string());
    }
}

fn resolve_visual_file(directory: &Path, file: &str) -> Option<SceneLayer> {
    let path = resolve_contained(directory, file)?;
    let visual = visual_kind(&path)?;
    layer_from_path(&path, visual)
}

fn layer_from_path(path: &Path, visual: SceneVisual) -> Option<SceneLayer> {
    Some(SceneLayer {
        url: file_url(path).ok()?,
        visual,
    })
}

fn visual_kind(path: &Path) -> Option<SceneVisual> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" => Some(SceneVisual::Image),
        "mp4" | "webm" | "mkv" => Some(SceneVisual::Video),
        _ => None,
    }
}

fn is_json_file(path: &Path) -> bool {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some(extension) => extension.eq_ignore_ascii_case("json"),
        None => false,
    }
}

fn is_scene_pkg(file: &str) -> bool {
    match Path::new(file.trim())
        .file_name()
        .and_then(|name| name.to_str())
    {
        Some(name) => name.eq_ignore_ascii_case("scene.pkg"),
        None => false,
    }
}

/// Join `file` onto `directory` when it names a file that stays inside.
///
/// This follows [`resolve_media`](wallpaper_import::resolve_media): absolute
/// paths and a `..` climb out of the project are rejected, and so is a
/// symlink whose canonical target leaves the project. The returned path is
/// the joined path, not the canonical one. `scene.pkg` is rejected before
/// the filesystem is touched.
fn resolve_contained(directory: &Path, file: &str) -> Option<PathBuf> {
    let file = file.trim();
    if file.is_empty() || is_scene_pkg(file) {
        return None;
    }
    let relative = Path::new(file);
    if !relative_file_stays_inside(relative) {
        return None;
    }
    let path = directory.join(relative);
    if !path.is_file() {
        return None;
    }
    match canonical_file_stays_inside(directory, &path) {
        Some(true) => Some(path),
        _ => None,
    }
}

/// `file` is a relative path that never climbs above the project directory.
///
/// Absolute paths are rejected because [`Path::join`] would discard the
/// project directory. A trailing climb such as `clips/../wallpaper.mp4` is
/// allowed when an earlier normal component keeps the result inside.
fn relative_file_stays_inside(file: &Path) -> bool {
    if file.is_absolute() {
        return false;
    }

    let mut depth = 0usize;
    for component in file.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }

    true
}

/// The opened file's canonical path is still inside the canonical project directory.
fn canonical_file_stays_inside(directory: &Path, file: &Path) -> Option<bool> {
    let directory = directory.canonicalize().ok()?;
    let canonical = file.canonicalize().ok()?;
    if canonical == directory {
        return Some(false);
    }
    Some(canonical.starts_with(directory))
}
