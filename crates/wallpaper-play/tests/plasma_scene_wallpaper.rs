use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wallpaper_import::scan_library;
use wallpaper_play::{
    install_plasma_scene_wallpaper, install_plasma_video_wallpaper, install_plasma_web_wallpaper,
    launch, plasma_scene_wallpaper_dir, plasma_scene_wallpaper_script, LaunchError, PlayError,
    PlayPlan, Players, SceneVisual, PLASMA_DBUS_METHOD, PLASMA_DBUS_PATH, PLASMA_DBUS_SERVICE,
    PLASMA_SCENE_WALLPAPER_PLUGIN, PLASMA_VIDEO_WALLPAPER_PLUGIN, PLASMA_WEB_WALLPAPER_PLUGIN,
};

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

fn scratch_dir() -> PathBuf {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("wallpaper-plasma-scene-{nanos}-{seq}"));
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

fn write_scene(dir: &Path, scene_json: &str) {
    write_project(
        dir,
        r#"{"type":"scene","file":"scene.json","title":"Synthetic Scene"}"#,
    );
    write_file(&dir.join("scene.json"), scene_json);
}

fn planned(root: &Path) -> Result<PlayPlan, PlayError> {
    let library = scan_library(root).expect("player scans the library");
    assert_eq!(library.len(), 1, "player sees one project");
    wallpaper_play::plan_playback(&library[0])
}

fn scene_layers(plan: &PlayPlan) -> &[wallpaper_play::SceneLayer] {
    match plan {
        PlayPlan::Scene { layers } => layers,
        other => panic!("expected a scene plan, got {other:?}"),
    }
}

fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

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

fn spawn_player(plan: &PlayPlan, players: &Players) -> Child {
    let mut busy = None;
    for _ in 0..50 {
        match launch(plan, players) {
            Ok(child) => return child,
            Err(LaunchError::Spawn { program, source })
                if source.kind() == ErrorKind::ExecutableFileBusy =>
            {
                busy = Some((program, source));
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("spawn player: {error}"),
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

fn scene_players(root: &Path, plasmashell: PathBuf, muted: bool) -> Players {
    let mut players = Players::default();
    players.plasmashell = plasmashell;
    players.video = root.join("mpv");
    players.web = root.join("xdg-open");
    players.plasma_data_home = root.join("data");
    players.muted = muted;
    players
}

fn assert_unplayable_scene(error: PlayError) {
    assert_eq!(error.to_string(), "wallpaper type `scene` cannot be played");
    match error {
        PlayError::Unsupported { kind } => assert_eq!(kind, "scene"),
        other => panic!("unexpected error: {other}"),
    }
}

fn assert_scene_package(data_home: &Path) {
    let installed = plasma_scene_wallpaper_dir(data_home);
    let metadata = fs::read_to_string(installed.join("metadata.json")).expect("installed metadata");
    assert!(metadata.contains(PLASMA_SCENE_WALLPAPER_PLUGIN));
    assert!(metadata.contains("Plasma/Wallpaper"));
    assert!(!metadata.contains(PLASMA_VIDEO_WALLPAPER_PLUGIN));
    assert!(!metadata.contains(PLASMA_WEB_WALLPAPER_PLUGIN));
    let qml = fs::read_to_string(installed.join("contents/ui/main.qml")).expect("installed qml");
    assert!(
        qml.contains("The first resolved visual fills the desktop"),
        "qml does not fill the desktop with the first layer"
    );
    assert!(qml.contains("anchors.fill: parent"), "{qml}");
    assert!(
        qml.contains("Repeater"),
        "qml does not show each configured layer"
    );
    assert!(qml.contains("Image"), "still layers are not shown");
    assert!(
        qml.contains("root.configuration.Layers"),
        "qml does not read the layer list"
    );
    assert!(
        qml.contains("MediaPlayer.Infinite"),
        "video textures do not loop"
    );
    assert!(
        qml.contains("audioOutput: modelData.kind === \"video\" ? audio : null"),
        "a still image has audio: {qml}"
    );
    assert!(
        qml.contains("Still images have no audio"),
        "qml does not say a still image has no audio"
    );
    assert!(
        qml.contains("root.configuration.Muted !== false"),
        "video mute does not follow Muted"
    );
    assert!(!qml.contains("mpv"), "scene package names mpv");
    assert!(!qml.contains("xdg-open"), "scene package names xdg-open");
}

fn direct_image_scene(project: &Path, image_name: &str) -> PathBuf {
    write_scene(
        project,
        &format!(
            r#"{{"objects":[{{"classname":"ImageLayer","image":"{image_name}","visible":true,"name":"layer"}}]}}"#
        ),
    );
    let image = project.join(image_name);
    write_file(&image, "synthetic image bytes");
    image
}

#[test]
fn visible_image_layer_plans_the_resolved_file_url() {
    let root = scratch_dir();
    let project = root.join("synthetic-scene");
    let image = direct_image_scene(&project, "layer.png");

    let plan = planned(&root).expect("scene plan");
    let layers = scene_layers(&plan);
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].visual, SceneVisual::Image);
    assert_eq!(layers[0].url, file_url(&image));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn picture_and_video_extensions_choose_the_layer_kind() {
    let root = scratch_dir();
    for (name, visual) in [
        ("still.png", SceneVisual::Image),
        ("still.jpg", SceneVisual::Image),
        ("still.jpeg", SceneVisual::Image),
        ("still.gif", SceneVisual::Image),
        ("still.webp", SceneVisual::Image),
        ("still.PNG", SceneVisual::Image),
        ("clip.mp4", SceneVisual::Video),
        ("clip.webm", SceneVisual::Video),
        ("clip.mkv", SceneVisual::Video),
    ] {
        let project = root.join(name);
        let media = direct_image_scene(&project, name);
        let library = scan_library(&root).expect("scan");
        let entry = library
            .iter()
            .find(|entry| entry.directory == project)
            .expect("entry");
        let plan = wallpaper_play::plan_playback(entry).expect(name);
        let layers = scene_layers(&plan);
        assert_eq!(layers.len(), 1, "{name}");
        assert_eq!(layers[0].visual, visual, "{name}");
        assert_eq!(layers[0].url, file_url(&media), "{name}");
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn model_and_material_resolve_to_the_texture_inside_the_project() {
    let root = scratch_dir();
    let project = root.join("model-scene");
    write_scene(
        &project,
        r#"{"objects":[{"classname":"ImageLayer","image":"models/layer.json","visible":true}]}"#,
    );
    write_file(
        &project.join("models/layer.json"),
        r#"{"autosize":true,"material":"materials/layer.json"}"#,
    );
    write_file(
        &project.join("materials/layer.json"),
        r#"{"passes":[{"shader":"genericimage2","textures":["materials/missing.png","materials/layer.png"]}]}"#,
    );
    let texture = project.join("materials/layer.png");
    write_file(&texture, "synthetic texture bytes");

    let plan = planned(&root).expect("model chain");
    let layers = scene_layers(&plan);
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].visual, SceneVisual::Image);
    assert_eq!(layers[0].url, file_url(&texture));
    assert!(
        !layers[0].url.contains("models/layer.json"),
        "played the model json: {}",
        layers[0].url
    );
    assert!(
        !layers[0].url.contains("materials/layer.json"),
        "played the material json: {}",
        layers[0].url
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn video_texture_loops_muted_and_sound_leaves_audio_on() {
    let root = scratch_dir();
    let project = root.join("video-texture");
    write_scene(
        &project,
        r#"{"objects":[{"classname":"ImageLayer","image":"models/clip.json","visible":true}]}"#,
    );
    write_file(
        &project.join("models/clip.json"),
        r#"{"material":"materials/clip.json"}"#,
    );
    write_file(
        &project.join("materials/clip.json"),
        r#"{"passes":[{"textures":["materials/clip.mp4"]}]}"#,
    );
    let video = project.join("materials/clip.mp4");
    write_file(&video, "synthetic video bytes");

    let plan = planned(&root).expect("video texture");
    let layers = scene_layers(&plan);
    assert_eq!(layers[0].visual, SceneVisual::Video);
    assert_eq!(layers[0].url, file_url(&video));

    let muted = plasma_scene_wallpaper_script(layers, true);
    assert!(muted.contains(&file_url(&video)), "{muted}");
    assert!(muted.contains(PLASMA_SCENE_WALLPAPER_PLUGIN), "{muted}");
    assert!(
        muted.contains("writeConfig(\"Muted\", true)"),
        "video texture is not muted: {muted}"
    );
    assert!(!muted.contains("mpv"), "{muted}");

    let sound = plasma_scene_wallpaper_script(layers, false);
    assert!(sound.contains(&file_url(&video)), "{sound}");
    assert!(
        sound.contains("writeConfig(\"Muted\", false)"),
        "sound-on script is still muted: {sound}"
    );
    assert!(
        !sound.contains("writeConfig(\"Muted\", true)"),
        "sound-on script also mutes: {sound}"
    );

    install_plasma_scene_wallpaper(&root.join("data")).expect("install");
    let qml = fs::read_to_string(
        plasma_scene_wallpaper_dir(&root.join("data")).join("contents/ui/main.qml"),
    )
    .expect("qml");
    assert!(qml.contains("MediaPlayer.Infinite"), "{qml}");
    assert!(
        qml.contains("audioOutput: modelData.kind === \"video\" ? audio : null"),
        "{qml}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn particle_system_is_ignored_and_the_image_still_plays() {
    let root = scratch_dir();
    let project = root.join("particles");
    write_scene(
        &project,
        r#"{"objects":[
            {"classname":"ParticleSystem","image":"particle.png","visible":true,"file":"particle.png"},
            {"classname":"ImageLayer","image":"layer.png","visible":true}
        ]}"#,
    );
    write_file(&project.join("particle.png"), "synthetic particle");
    let image = project.join("layer.png");
    write_file(&image, "synthetic image bytes");

    let plan = planned(&root).expect("image beside particles");
    let layers = scene_layers(&plan);
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].url, file_url(&image));
    assert!(!layers[0].url.contains("particle.png"));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn text_model_sound_script_and_hidden_layers_are_not_played() {
    let root = scratch_dir();
    let project = root.join("skipped");
    write_scene(
        &project,
        r#"{"objects":[
            {"classname":"TextLayer","text":"hello","visible":true,"image":"text.png"},
            {"classname":"Model","model":"models/figure.json","image":"models/figure.json","visible":true},
            {"classname":"Sound","file":"loop.mp3","visible":true,"image":"sound.png"},
            {"classname":"ImageLayer","image":"scripted.png","visible":true,"script":"scripts/user.js"},
            {"classname":"ImageLayer","image":"hidden.png","visible":false},
            {"classname":"ImageLayer","image":"shown.png","name":"shown"}
        ]}"#,
    );
    write_file(
        &project.join("models/figure.json"),
        r#"{"material":"materials/figure.json"}"#,
    );
    write_file(
        &project.join("materials/figure.json"),
        r#"{"passes":[{"textures":["materials/figure.png"]}]}"#,
    );
    for name in [
        "text.png",
        "sound.png",
        "scripted.png",
        "hidden.png",
        "materials/figure.png",
    ] {
        write_file(&project.join(name), "not played");
    }
    let shown = project.join("shown.png");
    write_file(&shown, "synthetic image bytes");

    let plan = planned(&root).expect("remaining image");
    let layers = scene_layers(&plan);
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].visual, SceneVisual::Image);
    assert_eq!(layers[0].url, file_url(&shown));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn scene_without_a_resolvable_layer_cannot_be_played() {
    let root = scratch_dir();
    let cases = [
        ("missing-scene", None, None),
        (
            "particles-only",
            Some(r#"{"objects":[{"classname":"ParticleSystem","visible":true}]}"#),
            None,
        ),
        (
            "model-only",
            Some(
                r#"{"objects":[{"classname":"Model","image":"models/figure.json","visible":true}]}"#,
            ),
            Some("models/figure.json"),
        ),
        (
            "script-only",
            Some(
                r#"{"objects":[{"classname":"ImageLayer","image":"layer.png","visible":true,"script":"scripts/user.js"}]}"#,
            ),
            Some("layer.png"),
        ),
        (
            "hidden-only",
            Some(r#"{"objects":[{"classname":"ImageLayer","image":"layer.png","visible":false}]}"#),
            Some("layer.png"),
        ),
        (
            "not-a-picture",
            Some(r#"{"objects":[{"classname":"ImageLayer","image":"notes.txt","visible":true}]}"#),
            Some("notes.txt"),
        ),
    ];
    for (name, scene, extra) in cases {
        let project = root.join(name);
        write_project(
            &project,
            r#"{"type":"scene","file":"scene.json","title":"Synthetic Scene"}"#,
        );
        if let Some(scene) = scene {
            write_file(&project.join("scene.json"), scene);
        }
        if let Some(extra) = extra {
            write_file(&project.join(extra), "synthetic bytes");
        }
        if name == "model-only" {
            write_file(
                &project.join("models/figure.json"),
                r#"{"material":"materials/figure.json"}"#,
            );
            write_file(
                &project.join("materials/figure.json"),
                r#"{"passes":[{"textures":["materials/figure.png"]}]}"#,
            );
            write_file(&project.join("materials/figure.png"), "synthetic texture");
        }
        let library = scan_library(&root).expect("scan");
        let entry = library
            .iter()
            .find(|entry| entry.directory == project)
            .expect(name);
        let error = wallpaper_play::plan_playback(entry).expect_err(name);
        assert_unplayable_scene(error);
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn escaped_absolute_and_symlink_paths_are_not_used() {
    let root = scratch_dir();
    let project = root.join("project");
    write_scene(
        &project,
        r#"{"objects":[{"classname":"ImageLayer","image":"../secret.png","visible":true}]}"#,
    );
    write_file(&root.join("secret.png"), "outside the project");
    assert_unplayable_scene(planned(&root).expect_err("climb"));

    write_scene(
        &project,
        r#"{"objects":[{"classname":"ImageLayer","image":"/tmp/secret.png","visible":true}]}"#,
    );
    assert_unplayable_scene(planned(&root).expect_err("absolute"));

    let outside = root.join("outside.png");
    write_file(&outside, "linked outside");
    std::os::unix::fs::symlink(&outside, project.join("link.png")).expect("symlink");
    write_scene(
        &project,
        r#"{"objects":[{"classname":"ImageLayer","image":"link.png","visible":true}]}"#,
    );
    assert_unplayable_scene(planned(&root).expect_err("symlink"));

    write_file(&project.join("clips/keep.txt"), "directory");
    write_file(&project.join("inside.png"), "stays inside");
    write_scene(
        &project,
        r#"{"objects":[{"classname":"ImageLayer","image":"clips/../inside.png","visible":true}]}"#,
    );
    let plan = planned(&root).expect("climb that stays inside");
    assert_eq!(
        scene_layers(&plan)[0].url,
        file_url(&project.join("clips/../inside.png"))
    );

    write_project(
        &project,
        r#"{"type":"scene","file":"../scene.json","title":"Escape"}"#,
    );
    write_file(
        &root.join("scene.json"),
        r#"{"objects":[{"classname":"ImageLayer","image":"secret.png","visible":true}]}"#,
    );
    assert_unplayable_scene(planned(&root).expect_err("scene json escape"));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn scene_pkg_is_not_read() {
    let root = scratch_dir();
    let project = root.join("packaged");
    write_project(
        &project,
        r#"{"type":"scene","file":"scene.pkg","title":"Synthetic Scene"}"#,
    );
    write_file(
        &project.join("scene.pkg"),
        r#"{"objects":[{"classname":"ImageLayer","image":"pkg.png","visible":true}]}"#,
    );
    write_file(
        &project.join("pkg.png"),
        "would play if the package was read",
    );
    assert_unplayable_scene(planned(&root).expect_err("pkg only"));

    write_file(
        &project.join("scene.json"),
        r#"{"objects":[{"classname":"ImageLayer","image":"unpacked.png","visible":true}]}"#,
    );
    let image = project.join("unpacked.png");
    write_file(&image, "synthetic image bytes");
    let plan = planned(&root).expect("unpacked scene.json");
    assert_eq!(scene_layers(&plan)[0].url, file_url(&image));
    assert!(!scene_layers(&plan)[0].url.contains("pkg.png"));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn nested_scene_file_from_project_json_is_used() {
    let root = scratch_dir();
    let project = root.join("nested");
    write_project(
        &project,
        r#"{"type":"scene","file":"scenes/room.json","title":"Nested Scene"}"#,
    );
    write_file(
        &project.join("scenes/room.json"),
        r#"{"objects":[{"classname":"ImageLayer","image":"room.png","visible":true}]}"#,
    );
    write_file(
        &project.join("scene.json"),
        r#"{"objects":[{"classname":"ImageLayer","image":"other.png","visible":true}]}"#,
    );
    let image = project.join("room.png");
    write_file(&image, "synthetic image bytes");
    write_file(&project.join("other.png"), "not this file");

    let plan = planned(&root).expect("nested scene file");
    assert_eq!(scene_layers(&plan)[0].url, file_url(&image));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn two_image_layers_are_named_and_the_first_fills_the_desktop() {
    let root = scratch_dir();
    let project = root.join("two-layers");
    write_scene(
        &project,
        r#"{"objects":[
            {"classname":"ParticleSystem","visible":true,"image":"skip.png"},
            {"classname":"ImageLayer","image":"first.png","visible":true},
            {"classname":"ImageLayer","image":"second.mp4","visible":true}
        ]}"#,
    );
    write_file(&project.join("skip.png"), "ignored");
    let first = project.join("first.png");
    let second = project.join("second.mp4");
    write_file(&first, "synthetic image bytes");
    write_file(&second, "synthetic video bytes");

    let plan = planned(&root).expect("two layers");
    let layers = scene_layers(&plan);
    assert_eq!(layers.len(), 2);
    assert_eq!(layers[0].visual, SceneVisual::Image);
    assert_eq!(layers[0].url, file_url(&first));
    assert_eq!(layers[1].visual, SceneVisual::Video);
    assert_eq!(layers[1].url, file_url(&second));

    let script = plasma_scene_wallpaper_script(layers, true);
    let first_at = script.find(&file_url(&first)).expect("first url");
    let second_at = script.find(&file_url(&second)).expect("second url");
    assert!(first_at < second_at, "{script}");
    assert!(script.contains(r#"\"kind\":\"image\""#), "{script}");
    assert!(script.contains(r#"\"kind\":\"video\""#), "{script}");
    assert!(script.contains(PLASMA_SCENE_WALLPAPER_PLUGIN), "{script}");
    assert!(!script.contains("skip.png"), "{script}");
    assert!(!script.contains("mpv"), "{script}");
    assert!(!script.contains("xdg-open"), "{script}");

    let plasma_record = root.join("plasma-argv");
    let mpv_record = root.join("mpv-argv");
    let open_record = root.join("xdg-argv");
    let plasmashell = write_argv_stub(&root, "qdbus6", &plasma_record);
    let mpv = write_argv_stub(&root, "mpv", &mpv_record);
    let xdg_open = write_argv_stub(&root, "xdg-open", &open_record);
    let mut players = scene_players(&root, plasmashell.clone(), true);
    players.video = mpv;
    players.web = xdg_open;
    players.plasma = false;
    players.web_plasma = false;

    let video = install_plasma_video_wallpaper(&players.plasma_data_home).expect("video");
    let web = install_plasma_web_wallpaper(&players.plasma_data_home).expect("web");

    let mut child = spawn_player(&plan, &players);
    let status = child.wait().expect("wait for plasmashell tool");
    assert!(status.success());
    assert_eq!(
        read_argv(&plasma_record),
        vec![
            plasmashell.display().to_string(),
            PLASMA_DBUS_SERVICE.to_string(),
            PLASMA_DBUS_PATH.to_string(),
            PLASMA_DBUS_METHOD.to_string(),
            script,
        ]
    );
    assert!(!mpv_record.exists(), "scene playback spawned mpv");
    assert!(!open_record.exists(), "scene playback spawned xdg-open");
    assert_scene_package(&players.plasma_data_home);
    assert!(video.join("metadata.json").is_file());
    assert!(web.join("metadata.json").is_file());
    let _ = fs::remove_dir_all(&root);
}

fn workshop_path(steam_root: &Path) -> PathBuf {
    steam_root
        .join("steamapps")
        .join("workshop")
        .join("content")
        .join("431960")
}

fn wallpaper_env(home: &Path, data_home: &Path, path: &Path, args: &[&str]) -> Output {
    let mut last = None;
    for _ in 0..50 {
        let output = Command::new(env!("CARGO_BIN_EXE_wallpaper"))
            .env("HOME", home)
            .env("XDG_DATA_HOME", data_home)
            .env("PATH", path)
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
    last.expect("busy wallpaper")
}

fn image_workshop(scratch: &Path) -> (PathBuf, PathBuf) {
    let steam_root = scratch.join("steam-root");
    let project = workshop_path(&steam_root).join("1001");
    let image = direct_image_scene(&project, "layer.png");
    (steam_root, image)
}

#[test]
fn wallpaper_play_scene_selects_the_plugin_and_spawns_nothing_else() {
    let scratch = scratch_dir();
    let (steam_root, image) = image_workshop(&scratch);
    let data_home = scratch.join("data");
    let bin = scratch.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let plasma_record = scratch.join("plasma-argv");
    let mpv_record = scratch.join("mpv-argv");
    let open_record = scratch.join("xdg-argv");
    let plasmashell = write_argv_stub(&bin, "qdbus6", &plasma_record);
    write_argv_stub(&bin, "mpv", &mpv_record);
    write_argv_stub(&bin, "xdg-open", &open_record);

    let steam_root = steam_root.display().to_string();
    let output = wallpaper_env(
        &scratch,
        &data_home,
        &bin,
        &["play", "1001", "--steam-root", &steam_root],
    );
    assert!(
        output.status.success(),
        "wallpaper failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let argv = read_argv(&plasma_record);
    assert!(
        argv[0] == "qdbus6" || argv[0] == plasmashell.display().to_string(),
        "plasmashell was not qdbus6: {}",
        argv[0]
    );
    assert_eq!(argv[1], PLASMA_DBUS_SERVICE);
    assert_eq!(argv[2], PLASMA_DBUS_PATH);
    assert_eq!(argv[3], PLASMA_DBUS_METHOD);
    let script = argv.last().expect("script argument");
    assert!(script.contains(&file_url(&image)), "{script}");
    assert!(script.contains(PLASMA_SCENE_WALLPAPER_PLUGIN), "{script}");
    assert!(script.contains("desktops()"), "{script}");
    assert!(script.contains("writeConfig(\"Muted\", true)"), "{script}");
    assert!(!script.contains("mpv"), "{script}");
    assert!(!script.contains("xdg-open"), "{script}");
    assert!(!script.contains("linux.wallpaper.video"), "{script}");
    assert!(!script.contains("linux.wallpaper.web"), "{script}");
    assert!(!mpv_record.exists(), "wallpaper play spawned mpv");
    assert!(!open_record.exists(), "wallpaper play spawned xdg-open");
    assert_scene_package(&data_home);
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn wallpaper_play_scene_sound_unmutes_a_video_texture() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("steam-root");
    let project = workshop_path(&steam_root).join("1001");
    write_scene(
        &project,
        r#"{"objects":[{"classname":"ImageLayer","image":"clip.mp4","visible":true}]}"#,
    );
    let video = project.join("clip.mp4");
    write_file(&video, "synthetic video bytes");

    let data_home = scratch.join("data");
    let bin = scratch.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let plasma_record = scratch.join("plasma-argv");
    let mpv_record = scratch.join("mpv-argv");
    write_argv_stub(&bin, "qdbus6", &plasma_record);
    write_argv_stub(&bin, "mpv", &mpv_record);

    let steam_root = steam_root.display().to_string();
    let output = wallpaper_env(
        &scratch,
        &data_home,
        &bin,
        &["play", "1001", "--sound", "--steam-root", &steam_root],
    );
    assert!(
        output.status.success(),
        "wallpaper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let script = read_argv(&plasma_record)
        .into_iter()
        .next_back()
        .expect("script");
    assert!(script.contains(&file_url(&video)), "{script}");
    assert!(script.contains(r#"\"kind\":\"video\""#), "{script}");
    assert!(script.contains("writeConfig(\"Muted\", false)"), "{script}");
    assert!(!mpv_record.exists(), "sound flag spawned mpv");
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn wallpaper_play_scene_player_flags_do_not_spawn_mpv_or_xdg_open() {
    let scratch = scratch_dir();
    let (steam_root, image) = image_workshop(&scratch);
    let data_home = scratch.join("data");
    let bin = scratch.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let plasma_record = scratch.join("plasma-argv");
    let mpv_record = scratch.join("mpv-argv");
    let open_record = scratch.join("xdg-argv");
    write_argv_stub(&bin, "qdbus6", &plasma_record);
    let mpv = write_argv_stub(&bin, "mpv", &mpv_record);
    let xdg_open = write_argv_stub(&bin, "xdg-open", &open_record);

    let steam_root = steam_root.display().to_string();
    let video_player = mpv.display().to_string();
    let web_player = xdg_open.display().to_string();
    let output = wallpaper_env(
        &scratch,
        &data_home,
        &bin,
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
    assert!(
        output.status.success(),
        "wallpaper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let script = read_argv(&plasma_record)
        .into_iter()
        .next_back()
        .expect("script");
    assert!(script.contains(&file_url(&image)), "{script}");
    assert!(script.contains(PLASMA_SCENE_WALLPAPER_PLUGIN), "{script}");
    assert!(!mpv_record.exists(), "--video-player spawned mpv");
    assert!(!open_record.exists(), "--web-player spawned xdg-open");
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn wallpaper_play_missing_scene_spawns_nothing() {
    let scratch = scratch_dir();
    let steam_root = scratch.join("steam-root");
    let project = workshop_path(&steam_root).join("1001");
    write_project(
        &project,
        r#"{"type":"scene","file":"scene.json","title":"Synthetic Scene"}"#,
    );
    let data_home = scratch.join("data");
    let bin = scratch.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let plasma_record = scratch.join("plasma-argv");
    let mpv_record = scratch.join("mpv-argv");
    let open_record = scratch.join("xdg-argv");
    write_argv_stub(&bin, "qdbus6", &plasma_record);
    write_argv_stub(&bin, "mpv", &mpv_record);
    write_argv_stub(&bin, "xdg-open", &open_record);

    let steam_root = steam_root.display().to_string();
    let output = wallpaper_env(
        &scratch,
        &data_home,
        &bin,
        &["play", "1001", "--steam-root", &steam_root],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "missing scene exited 0: {stderr}");
    assert!(
        stderr.contains("wallpaper type `scene` cannot be played"),
        "{stderr}"
    );
    assert!(!plasma_record.exists(), "missing scene called plasmashell");
    assert!(!mpv_record.exists(), "missing scene spawned mpv");
    assert!(!open_record.exists(), "missing scene spawned xdg-open");
    assert!(
        !data_home
            .join("plasma/wallpapers/linux.wallpaper.scene")
            .exists(),
        "missing scene installed the plugin"
    );
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn missing_plasmashell_for_a_scene_does_not_spawn_mpv() {
    let root = scratch_dir();
    let project = root.join("synthetic-scene");
    direct_image_scene(&project, "layer.png");
    let plan = planned(&root).expect("scene plan");
    let mpv_record = root.join("mpv-argv");
    let open_record = root.join("xdg-argv");
    let mpv = write_argv_stub(&root, "mpv", &mpv_record);
    let xdg_open = write_argv_stub(&root, "xdg-open", &open_record);
    let missing = root.join("no-such-plasmashell");
    let mut players = scene_players(&root, missing.clone(), true);
    players.video = mpv;
    players.web = xdg_open;

    let error = launch(&plan, &players).expect_err("missing plasmashell");
    assert!(
        error.to_string().contains("plasmashell tool not found"),
        "{error}"
    );
    match error {
        LaunchError::MissingPlasmashell { program, .. } => assert_eq!(program, missing),
        other => panic!("unexpected error: {other}"),
    }
    assert!(!mpv_record.exists(), "missing plasmashell spawned mpv");
    assert!(
        !open_record.exists(),
        "missing plasmashell spawned xdg-open"
    );
    let _ = fs::remove_dir_all(&root);
}
