# Install wallpaper on Linux

`wallpaper` plays video and web Wallpaper Engine workshop items that you already own. It reads the files Steam downloaded to your disk. It hands each item to `mpv` or `xdg-open`.

This project is not Wallpaper Engine. It is not affiliated with Wallpaper Engine or Valve. It does not download workshop content. Scene wallpapers do not play.

## Before you start

- Steam has Wallpaper Engine (app id 431960) installed.
- Steam has downloaded the workshop items you subscribe to.
- You have a clone of this repository.

## Install on Fedora

1. Install the build tools and players:

   ```sh
   sudo dnf install cargo rust gcc mpv xdg-utils
   ```

2. Build and install the command from the repository root:

   ```sh
   cargo install --path crates/wallpaper-play
   ```

3. Check that `~/.cargo/bin` is on your `PATH`:

   ```sh
   command -v wallpaper
   ```

   If this prints nothing, add `export PATH="$HOME/.cargo/bin:$PATH"` to `~/.bashrc`. Open a new terminal.

## Install on other Linux distributions

1. Install Rust with your package manager or with [rustup](https://rustup.rs).
2. Install `mpv` and `xdg-utils`.

   On Debian and Ubuntu:

   ```sh
   sudo apt install cargo mpv xdg-utils
   ```

   On Arch Linux:

   ```sh
   sudo pacman -S rust mpv xdg-utils
   ```

3. Follow steps 2 and 3 of the Fedora section.

## List your wallpapers

Run:

```sh
wallpaper list
```

Each line shows one workshop item: id, type, and title. Lines are sorted by id.

## Play a wallpaper

1. Copy an id from `wallpaper list`.
2. Play it:

   ```sh
   wallpaper play 1234567890
   ```

The command waits until the player exits. A scene item, or an id that is not in your library, exits with status 1 and starts no player.

To play the first video or web item in the library, leave out the id:

```sh
wallpaper play
```

## Point at a different Steam folder

Without `--steam-root`, `wallpaper` checks `~/.steam/steam`, `~/.local/share/Steam`, and `~/.steam/root`. For each root it looks in `steamapps/workshop/content/431960`, then in the libraries listed in `steamapps/libraryfolders.vdf`. It uses the first match.

If Steam lives somewhere else, pass the root. Flatpak Steam is one example:

```sh
wallpaper list --steam-root ~/.var/app/com.valvesoftware.Steam/.local/share/Steam
```

`--steam-root` reads that root and its `libraryfolders.vdf`. It skips the folders under `HOME`.

## Use a different player

`play` uses `mpv` for video items and `xdg-open` for web items. Pass a path to override either one:

```sh
wallpaper play 1234567890 --video-player /usr/bin/vlc
wallpaper play 1234567890 --web-player /usr/bin/firefox
```

## License

MIT
