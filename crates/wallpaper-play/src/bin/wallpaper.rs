//! Find a local Steam library, list its workshop items, and play one.

use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ExitCode, ExitStatus};

use wallpaper_import::{scan_library, workshop_dir, ImportError, LibraryEntry, Wallpaper};
use wallpaper_play::steam_root::{find_steam_root, find_workshop_root, SteamRootError};
use wallpaper_play::{
    play_entry, play_first, LaunchError, PlayEntryError, PlayError, PlayFirstError, Players,
    PARTIAL_SCENE_NOTICE,
};

const USAGE: &str = "\
Usage: wallpaper list [--steam-root DIR]
       wallpaper play [ID] [--steam-root DIR] [--sound] [--plasmashell PATH]
                        [--video-player PATH] [--web-player PATH]
       wallpaper ui [--steam-root DIR] [--sound] [--plasmashell PATH]
                      [--video-player PATH] [--web-player PATH]
       wallpaper desktop [--steam-root DIR] [--sound] [--plasmashell PATH]
                           [--video-player PATH] [--web-player PATH]

list prints one workshop item per line: id, type, and title, sorted by id.
play ID sets that video, web, or scene item as a KDE Plasma wallpaper.
When the Wallpaper Engine for KDE plugin is installed, a scene is handed to
that plugin, including a project that is only scene.pkg. When it is not, a
scene with an image or video layer stays a partial view and this command
says so. A scene with nothing to show, or an id that is not in the library,
exits with an error and does not start a player.
play with no ID uses the first video or web wallpaper in sorted path order.
It does not select a scene. A video item is muted and looped behind the
desktop icons on every desktop Plasma scripting can see. A web item loads
that page in Qt WebEngine the same way. A scene item without the live plugin
shows its visible image layers. A video texture loops. A still image has no
audio. --sound leaves video audio on. Particles, text, models, and scripts
in a scene are not played by the partial view.
--plasmashell is the qdbus tool. It calls
org.kde.PlasmaShell.evaluateScript. The default is the first of qdbus6,
qdbus-qt6, and qdbus that is on PATH. A missing plasmashell tool is an error
and does not start mpv or xdg-open.
--video-player PATH skips the Plasma wallpaper for a video item and runs
PATH with mpv arguments: an infinite file loop and the media path. It does
not change web or scene playback. The default program for that override is mpv.
--web-player PATH skips the Plasma wallpaper for a web item and runs PATH
with the file URL only. It does not change scene playback. The default
program for that override is xdg-open.
ui binds to 127.0.0.1, prints the page URL, and serves the workshop library.
The page plays a video or web item the same way play does. A scene item is
not offered on the page. An id that is not in the library is an error and
does not start a player.
Without --steam-root, search ~/.steam/steam, ~/.local/share/Steam, and
~/.steam/root under HOME. Each root is checked for
steamapps/workshop/content/431960, then for libraries named in
steamapps/libraryfolders.vdf. The first match is used.
--steam-root DIR uses that root and its libraryfolders.vdf, and does not
search HOME.
desktop opens the same library page in a window. Without a display it
serves that page and does not open a window. The page can add one extra
folder of Wallpaper Engine projects and choose whether to start on login.";

fn main() -> ExitCode {
    match run(env::args().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("wallpaper: {error}");
            ExitCode::from(1)
        }
    }
}

enum Command {
    List {
        steam_root: Option<PathBuf>,
    },
    Play {
        id: Option<String>,
        steam_root: Option<PathBuf>,
        video_player: Option<PathBuf>,
        web_player: Option<PathBuf>,
        plasmashell: Option<PathBuf>,
        sound: bool,
    },
    Ui {
        steam_root: Option<PathBuf>,
        video_player: Option<PathBuf>,
        web_player: Option<PathBuf>,
        plasmashell: Option<PathBuf>,
        sound: bool,
    },
    Desktop {
        args: Vec<String>,
    },
}

fn run(args: impl IntoIterator<Item = String>) -> Result<(), CliError> {
    match parse_args(args)? {
        Command::Desktop { args } => run_desktop(&args),
        Command::List { steam_root } => {
            let library = resolve_library(steam_root)?;
            list_projects(&library)
        }
        Command::Play {
            id,
            steam_root,
            video_player,
            web_player,
            plasmashell,
            sound,
        } => {
            let library = resolve_library(steam_root)?;
            let players = players_from_flags(video_player, web_player, plasmashell, sound);
            let mut child = match id {
                Some(id) => play_id(&library, &id, &players)?,
                None => play_first(&library, &players)?,
            };
            let status = child.wait().map_err(CliError::Wait)?;
            if status.success() {
                Ok(())
            } else {
                Err(CliError::PlayerExit(status))
            }
        }
        Command::Ui {
            steam_root,
            video_player,
            web_player,
            plasmashell,
            sound,
        } => {
            let library = match resolve_library(steam_root) {
                Ok(path) => Some(path),
                Err(CliError::MissingHome | CliError::SteamRoot(_) | CliError::Workshop(_)) => None,
                Err(error) => return Err(error),
            };
            let players = players_from_flags(video_player, web_player, plasmashell, sound);
            wallpaper_play::ui::serve(library, players).map_err(CliError::Serve)?;
            Ok(())
        }
    }
}

/// Replace this process with the desktop window program.
///
/// The desktop crate cannot depend back on this binary, so `wallpaper desktop`
/// execs the sibling `wallpaper-desktop` built next to it.
fn run_desktop(args: &[String]) -> Result<(), CliError> {
    let program = desktop_program();
    let error = std::process::Command::new(&program).args(args).exec();
    Err(CliError::DesktopSpawn {
        program,
        source: error,
    })
}

fn desktop_program() -> PathBuf {
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("wallpaper-desktop");
            if sibling.is_file() {
                return sibling;
            }
        }
    }
    PathBuf::from("wallpaper-desktop")
}

fn players_from_flags(
    video_player: Option<PathBuf>,
    web_player: Option<PathBuf>,
    plasmashell: Option<PathBuf>,
    sound: bool,
) -> Players {
    let mut players = Players::default();
    if let Some(video) = video_player {
        players.video = video;
        players.plasma = false;
    }
    if let Some(web) = web_player {
        players.web = web;
        players.web_plasma = false;
    }
    if let Some(tool) = plasmashell {
        players.plasmashell = tool;
    }
    if sound {
        players.muted = false;
    }
    players
}

fn resolve_library(steam_root: Option<PathBuf>) -> Result<PathBuf, CliError> {
    let steam_root = match steam_root {
        Some(root) => find_workshop_root(&root).unwrap_or(root),
        None => {
            let home = env::var_os("HOME").ok_or(CliError::MissingHome)?;
            find_steam_root(home)?
        }
    };
    Ok(workshop_dir(&steam_root)?)
}

fn list_projects(library: &Path) -> Result<(), CliError> {
    let mut entries = scan_library(library)?;
    entries.sort_by(|left, right| project_id(left).cmp(&project_id(right)));
    for entry in &entries {
        println!(
            "{} {} {}",
            project_id(entry),
            project_type(&entry.wallpaper),
            project_title(entry)
        );
    }
    Ok(())
}

fn play_id(library: &Path, id: &str, players: &Players) -> Result<Child, CliError> {
    let entries = scan_library(library)?;
    let entry = entries
        .iter()
        .find(|entry| project_id(entry) == id)
        .ok_or_else(|| CliError::UnknownId(id.to_string()))?;
    let played = play_entry(entry, players)?;
    if played.partial_scene {
        eprintln!("wallpaper: {PARTIAL_SCENE_NOTICE}");
    }
    Ok(played.child)
}

fn project_id(entry: &LibraryEntry) -> String {
    entry
        .directory
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn project_type(wallpaper: &Wallpaper) -> &str {
    match wallpaper {
        Wallpaper::Video { .. } => "video",
        Wallpaper::Web { .. } => "web",
        Wallpaper::Unsupported { kind } => kind.as_str(),
    }
}

/// Title from the classified project, or from `project.json` when the type
/// does not keep one.
fn project_title(entry: &LibraryEntry) -> String {
    match &entry.wallpaper {
        Wallpaper::Video { title, .. } | Wallpaper::Web { title, .. } => title.clone(),
        Wallpaper::Unsupported { .. } => title_from_project_json(&entry.directory),
    }
}

fn title_from_project_json(directory: &Path) -> String {
    let Ok(text) = fs::read_to_string(directory.join("project.json")) else {
        return String::new();
    };
    json_string_field(&text, "title")
        .map(|title| title.trim().to_string())
        .unwrap_or_default()
}

/// Value of a JSON string field, without pulling in a parser crate.
fn json_string_field(text: &str, field: &str) -> Option<String> {
    let pattern = format!("\"{field}\"");
    let mut rest = text;
    while let Some(index) = rest.find(&pattern) {
        let after_key = rest[index + pattern.len()..].trim_start();
        if let Some(after_colon) = after_key.strip_prefix(':') {
            if let Some(parsed) = parse_json_string(after_colon.trim_start()) {
                return Some(parsed);
            }
        }
        rest = &rest[index + pattern.len()..];
    }
    None
}

fn parse_json_string(value: &str) -> Option<String> {
    let mut chars = value.chars();
    if chars.next() != Some('"') {
        return None;
    }
    let mut out = String::new();
    while let Some(character) = chars.next() {
        match character {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'u' => {
                    let mut hex = String::new();
                    for _ in 0..4 {
                        hex.push(chars.next()?);
                    }
                    let code = u32::from_str_radix(&hex, 16).ok()?;
                    out.push(char::from_u32(code)?);
                }
                other => out.push(other),
            },
            other => out.push(other),
        }
    }
    None
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Command, CliError> {
    let mut args = args.into_iter();
    let command = match args.next().as_deref() {
        Some("list") => "list",
        Some("play") => "play",
        Some("ui") => "ui",
        Some("desktop") => {
            return Ok(Command::Desktop {
                args: args.collect(),
            });
        }
        Some(other) => {
            return Err(CliError::Usage(format!(
                "unknown command `{other}`\n{USAGE}"
            )));
        }
        None => return Err(CliError::Usage(USAGE.to_string())),
    };

    let mut steam_root = None;
    let mut video_player = None;
    let mut web_player = None;
    let mut plasmashell = None;
    let mut sound = false;
    let mut id = None;
    let rest: Vec<String> = args.collect();
    let mut index = 0;
    while index < rest.len() {
        let arg = &rest[index];
        if arg == "--sound" && matches!(command, "play" | "ui") {
            sound = true;
            index += 1;
            continue;
        }
        if let Some(flag) = arg.strip_prefix("--") {
            let value = rest
                .get(index + 1)
                .ok_or_else(|| CliError::Usage(format!("missing value for --{flag}\n{USAGE}")))?;
            match (command, flag) {
                (_, "steam-root") => steam_root = Some(PathBuf::from(value)),
                ("play" | "ui", "video-player") => video_player = Some(PathBuf::from(value)),
                ("play" | "ui", "web-player") => web_player = Some(PathBuf::from(value)),
                ("play" | "ui", "plasmashell") => plasmashell = Some(PathBuf::from(value)),
                _ => {
                    return Err(CliError::Usage(format!(
                        "unknown argument `--{flag}`\n{USAGE}"
                    )));
                }
            }
            index += 2;
            continue;
        }
        if command != "play" || id.is_some() {
            return Err(CliError::Usage(format!(
                "unknown argument `{arg}`\n{USAGE}"
            )));
        }
        id = Some(arg.clone());
        index += 1;
    }

    if command == "list" {
        Ok(Command::List { steam_root })
    } else if command == "ui" {
        Ok(Command::Ui {
            steam_root,
            video_player,
            web_player,
            plasmashell,
            sound,
        })
    } else {
        Ok(Command::Play {
            id,
            steam_root,
            video_player,
            web_player,
            plasmashell,
            sound,
        })
    }
}

#[derive(Debug)]
enum CliError {
    Usage(String),
    MissingHome,
    SteamRoot(SteamRootError),
    Workshop(ImportError),
    Play(PlayFirstError),
    Item(PlayError),
    Launch(LaunchError),
    UnknownId(String),
    Wait(io::Error),
    PlayerExit(ExitStatus),
    Serve(io::Error),
    DesktopSpawn { program: PathBuf, source: io::Error },
}

impl From<SteamRootError> for CliError {
    fn from(source: SteamRootError) -> Self {
        CliError::SteamRoot(source)
    }
}

impl From<ImportError> for CliError {
    fn from(source: ImportError) -> Self {
        CliError::Workshop(source)
    }
}

impl From<PlayFirstError> for CliError {
    fn from(source: PlayFirstError) -> Self {
        CliError::Play(source)
    }
}

impl From<PlayError> for CliError {
    fn from(source: PlayError) -> Self {
        CliError::Item(source)
    }
}

impl From<PlayEntryError> for CliError {
    fn from(source: PlayEntryError) -> Self {
        match source {
            PlayEntryError::Play(source) => CliError::Item(source),
            PlayEntryError::Launch(source) => CliError::Launch(source),
        }
    }
}

impl From<LaunchError> for CliError {
    fn from(source: LaunchError) -> Self {
        CliError::Launch(source)
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::Usage(message) => write!(f, "{message}"),
            CliError::MissingHome => {
                write!(f, "HOME is not set; pass --steam-root or set HOME")
            }
            CliError::SteamRoot(source) => write!(f, "{source}"),
            CliError::Workshop(source) => write!(f, "{source}"),
            CliError::Play(source) => write!(f, "{source}"),
            CliError::Item(source) => write!(f, "{source}"),
            CliError::Launch(source) => write!(f, "{source}"),
            CliError::UnknownId(id) => write!(f, "unknown wallpaper id `{id}`"),
            CliError::Wait(source) => write!(f, "failed to wait for player: {source}"),
            CliError::PlayerExit(status) => write!(f, "player exited with {status}"),
            CliError::Serve(source) => write!(f, "failed to serve the library page: {source}"),
            CliError::DesktopSpawn { program, source } => {
                write!(f, "failed to start {}: {source}", program.display())
            }
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CliError::SteamRoot(source) => Some(source),
            CliError::Workshop(source) => Some(source),
            CliError::Play(source) => Some(source),
            CliError::Item(source) => Some(source),
            CliError::Launch(source) => Some(source),
            CliError::Wait(source) => Some(source),
            CliError::Serve(source) => Some(source),
            CliError::DesktopSpawn { source, .. } => Some(source),
            _ => None,
        }
    }
}
