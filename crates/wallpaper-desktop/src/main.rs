//! `wallpaper desktop` — the library window.

use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    match wallpaper_desktop::run_from_env(env::args().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("wallpaper: {error}");
            ExitCode::from(1)
        }
    }
}
