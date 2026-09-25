//! Find a local Steam library and play the first video or web wallpaper.

use std::env;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::process::{ExitCode, ExitStatus};

use wallpaper_import::{workshop_dir, ImportError};
use wallpaper_play::steam_root::{find_steam_root, SteamRootError};
use wallpaper_play::{play_first, PlayFirstError, Players};

const USAGE: &str = "\
Usage: wallpaper play [--steam-root DIR] [--video-player PATH] [--web-player PATH]

Play the first video or web wallpaper in a Steam workshop library.
Without --steam-root, search ~/.steam/steam, ~/.local/share/Steam, and
~/.steam/root under HOME, and use the first root that contains
steamapps/workshop/content/431960.
--video-player defaults to mpv. --web-player defaults to xdg-open.";

fn main() -> ExitCode {
    match play_from_args(env::args().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("wallpaper: {error}");
            ExitCode::from(1)
        }
    }
}

struct PlayArgs {
    steam_root: Option<PathBuf>,
    video_player: Option<PathBuf>,
    web_player: Option<PathBuf>,
}

fn play_from_args(args: impl IntoIterator<Item = String>) -> Result<(), CliError> {
    let args = parse_play_args(args)?;
    let steam_root = match args.steam_root {
        Some(root) => root,
        None => {
            let home = env::var_os("HOME").ok_or(CliError::MissingHome)?;
            find_steam_root(home)?
        }
    };
    let library = workshop_dir(&steam_root)?;

    let mut players = Players::default();
    if let Some(video) = args.video_player {
        players.video = video;
    }
    if let Some(web) = args.web_player {
        players.web = web;
    }

    let mut child = play_first(&library, &players)?;
    let status = child.wait().map_err(CliError::Wait)?;
    if status.success() {
        Ok(())
    } else {
        Err(CliError::PlayerExit(status))
    }
}

fn parse_play_args(args: impl IntoIterator<Item = String>) -> Result<PlayArgs, CliError> {
    let mut args = args.into_iter();
    match args.next().as_deref() {
        Some("play") => {}
        Some(other) => {
            return Err(CliError::Usage(format!(
                "unknown command `{other}`\n{USAGE}"
            )));
        }
        None => return Err(CliError::Usage(USAGE.to_string())),
    }

    let mut parsed = PlayArgs {
        steam_root: None,
        video_player: None,
        web_player: None,
    };

    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| CliError::Usage(format!("missing value for {flag}\n{USAGE}")))?;
        match flag.as_str() {
            "--steam-root" => parsed.steam_root = Some(PathBuf::from(value)),
            "--video-player" => parsed.video_player = Some(PathBuf::from(value)),
            "--web-player" => parsed.web_player = Some(PathBuf::from(value)),
            other => {
                return Err(CliError::Usage(format!(
                    "unknown argument `{other}`\n{USAGE}"
                )));
            }
        }
    }

    Ok(parsed)
}

enum CliError {
    Usage(String),
    MissingHome,
    SteamRoot(SteamRootError),
    Workshop(ImportError),
    Play(PlayFirstError),
    Wait(io::Error),
    PlayerExit(ExitStatus),
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
            CliError::Wait(source) => write!(f, "failed to wait for player: {source}"),
            CliError::PlayerExit(status) => write!(f, "player exited with {status}"),
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CliError::SteamRoot(source) => Some(source),
            CliError::Workshop(source) => Some(source),
            CliError::Play(source) => Some(source),
            CliError::Wait(source) => Some(source),
            _ => None,
        }
    }
}
