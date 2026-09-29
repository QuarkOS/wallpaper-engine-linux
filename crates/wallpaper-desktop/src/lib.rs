//! Native window around the local wallpaper library.
//!
//! [`run`] serves the library page on `127.0.0.1` and, when a display is
//! available, opens it in a tao/wry system webview. Without a display the
//! server stays up and no window is opened. Tests use [`serve`] and the
//! settings functions, which never create a window.
//!
//! Settings live in `$XDG_CONFIG_HOME/wallpaper/settings.json`. One extra
//! folder of Wallpaper Engine projects is stored there and included on the
//! next scan. The first launch asks about starting on login. Yes writes an
//! XDG autostart file that runs `wallpaper desktop`.

mod library;
mod server;
mod settings;
mod window;

use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::thread;

pub use library::{collect_library, resolve_workshop, LibraryCard};
pub use server::{serve, DesktopOptions, Server};
pub use settings::{
    autostart_desktop_path, default_config_home, load_settings, save_settings, set_autostart,
    set_extra_library, settings_path, Settings, AUTOSTART_DESKTOP,
};
pub use window::{display_available, open_library_window};

use wallpaper_play::Players;

/// Failure while starting the desktop app.
#[derive(Debug)]
pub enum DesktopError {
    Usage(String),
    Serve(io::Error),
    Window(String),
}

impl std::fmt::Display for DesktopError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DesktopError::Usage(message) => write!(formatter, "{message}"),
            DesktopError::Serve(source) => {
                write!(formatter, "failed to serve the library page: {source}")
            }
            DesktopError::Window(message) => {
                write!(formatter, "failed to open the window: {message}")
            }
        }
    }
}

impl std::error::Error for DesktopError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DesktopError::Serve(source) => Some(source),
            _ => None,
        }
    }
}

const USAGE: &str = "\
Usage: wallpaper desktop [--steam-root DIR] [--sound] [--plasmashell PATH]
                         [--video-player PATH] [--web-player PATH]

desktop opens the workshop library in a window. Without a display it serves
the page and does not open a window.
--video-player PATH skips the Plasma wallpaper and runs PATH.
--web-player defaults to xdg-open.";

/// Parse desktop flags from the environment and serve, opening a window when a display exists.
pub fn run_from_env(args: impl IntoIterator<Item = String>) -> Result<(), DesktopError> {
    let options = options_from_args(args)?;
    run(options)
}

/// Serve `options` and open a window when [`display_available`] is true.
///
/// The URL is printed before any window is created. With no display the
/// calling thread stays parked so the server keeps running.
pub fn run(options: DesktopOptions) -> Result<(), DesktopError> {
    let server = serve(options).map_err(DesktopError::Serve)?;
    let url = server.url();
    println!("{url}");
    let _ = io::stdout().flush();
    if display_available() {
        open_library_window(&url).map_err(DesktopError::Window)?;
    } else {
        thread::park();
    }
    Ok(())
}

fn options_from_args(
    args: impl IntoIterator<Item = String>,
) -> Result<DesktopOptions, DesktopError> {
    let mut steam_root = None;
    let mut video_player = None;
    let mut web_player = None;
    let mut plasmashell = None;
    let mut sound = false;
    let rest: Vec<String> = args.into_iter().collect();
    let mut index = 0;
    while index < rest.len() {
        let arg = &rest[index];
        if arg == "--sound" {
            sound = true;
            index += 1;
            continue;
        }
        if let Some(flag) = arg.strip_prefix("--") {
            let value = rest.get(index + 1).ok_or_else(|| {
                DesktopError::Usage(format!("missing value for --{flag}\n{USAGE}"))
            })?;
            match flag {
                "steam-root" => steam_root = Some(PathBuf::from(value)),
                "video-player" => video_player = Some(PathBuf::from(value)),
                "web-player" => web_player = Some(PathBuf::from(value)),
                "plasmashell" => plasmashell = Some(PathBuf::from(value)),
                _ => {
                    return Err(DesktopError::Usage(format!(
                        "unknown argument `--{flag}`\n{USAGE}"
                    )));
                }
            }
            index += 2;
            continue;
        }
        return Err(DesktopError::Usage(format!(
            "unknown argument `{arg}`\n{USAGE}"
        )));
    }

    let mut players = Players::default();
    if let Some(video) = video_player {
        players.video = video;
        players.plasma = false;
    }
    if let Some(web) = web_player {
        players.web = web;
    }
    if let Some(tool) = plasmashell {
        players.plasmashell = tool;
    }
    if sound {
        players.muted = false;
    }

    Ok(DesktopOptions {
        steam_root,
        home: env::var_os("HOME").map(PathBuf::from),
        config_home: default_config_home(),
        players,
    })
}
