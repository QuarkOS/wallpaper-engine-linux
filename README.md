# Install wallpaper on Linux

`wallpaper` plays video, web, and scene Wallpaper Engine workshop items that you already own. It reads the files Steam downloaded to your disk. A video item becomes a muted KDE Plasma wallpaper behind the desktop icons. A web item becomes a muted KDE Plasma wallpaper the same way, loaded in Qt WebEngine. Image layers in a scene can show on the desktop.

This project is not Wallpaper Engine. It is not affiliated with Wallpaper Engine or Valve. It does not download workshop content. Particles, text, models, and scripts still cannot.

## Before you start

- Steam has Wallpaper Engine (app id 431960) installed.
- Steam has downloaded the workshop items you subscribe to.
- You have a clone of this repository.

## Install on Fedora

1. Install the build tools, the Plasma shell scripting tool, Qt Multimedia, and Qt WebEngine:

   ```sh
   sudo dnf install cargo rust gcc qt6-qttools qt6-qtmultimedia qt6-qtwebengine xdg-utils
   ```

   `mpv` is only needed when you pass `--video-player`. `xdg-utils` is only needed when you pass `--web-player`.

2. Build and install the command from the repository root:

   ```sh
   cargo install --path crates/wallpaper-play
   ```

3. Check that `~/.cargo/bin` is on your `PATH`:

   ```sh
   command -v wallpaper
   ```

   If this prints nothing, add `export PATH="$HOME/.cargo/bin:$PATH"` to `~/.bashrc`. Open a new terminal.

## Fedora RPM

Build an RPM from this repository, then install it. The package provides `wallpaper`, `wallpaper-desktop`, the monochrome stylesheet, and a menu entry that runs `wallpaper desktop`.

```sh
sudo dnf install rpm-build rust cargo gcc pkgconf-pkg-config gtk3-devel webkit2gtk4.1-devel curl ca-certificates
mkdir -p "$HOME/rpmbuild/SOURCES"
tar -czf "$HOME/rpmbuild/SOURCES/wallpaper-engine-linux-0.1.0.tar.gz" \
  --exclude target \
  --exclude .git \
  --transform 's,^,wallpaper-engine-linux-0.1.0/,' \
  .gitignore Cargo.lock Cargo.toml README.md rust-toolchain.toml crates packaging ui
rpmbuild -bb packaging/fedora/wallpaper-engine-linux.spec
sudo dnf install "$HOME/rpmbuild/RPMS/$(uname -m)/wallpaper-engine-linux-0.1.0-1"*.rpm
```

If Fedora's `cargo` is older than Rust 1.98.1, the spec installs that toolchain with rustup while it builds.

## Install on other Linux distributions

1. Install Rust with your package manager or with [rustup](https://rustup.rs).
2. Install Qt 6 Multimedia, Qt 6 WebEngine, and a Qt 6 `qdbus` tool (`qdbus6`, `qdbus-qt6`, or `qdbus`). Install `mpv` only if you want `--video-player`. Install `xdg-utils` only if you want `--web-player`.

   On Debian and Ubuntu, the Qt 6 packages are `qt6-multimedia`, `qt6-webengine`, and the package that provides `qdbus6`.

   On Arch Linux:

   ```sh
   sudo pacman -S rust qt6-tools qt6-multimedia qt6-webengine
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

The command waits until the plasmashell tool exits. A scene with no image or video layer, or an id that is not in your library, exits with status 1 and starts no player.

To play the first video or web item in the library, leave out the id. That form does not select a scene:

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

## Video wallpapers

On KDE Plasma 6, `play` installs a small video wallpaper plugin into `~/.local/share/plasma/wallpapers/linux.wallpaper.video` (or `$XDG_DATA_HOME/plasma/wallpapers/linux.wallpaper.video`) and selects it with Plasma Shell scripting:

```sh
qdbus6 org.kde.plasmashell /PlasmaShell org.kde.PlasmaShell.evaluateScript
```

Plasma 6.7 does not ship a stock video wallpaper plugin. The installed plugin loops the file with Qt Multimedia. The script sets that plugin on every desktop `desktops()` returns. Audio is muted. Pass `--sound` to leave it on.

`--video-player PATH` skips the Plasma wallpaper for that video and runs the program with `mpv` arguments instead. It does not change web or scene playback.

```sh
wallpaper play 1234567890 --sound
wallpaper play 1234567890 --video-player /usr/bin/mpv
```

## Web wallpapers

On KDE Plasma 6, `play` installs a web wallpaper plugin into `~/.local/share/plasma/wallpapers/linux.wallpaper.web` (or `$XDG_DATA_HOME/plasma/wallpapers/linux.wallpaper.web`) and selects it with the same Plasma Shell scripting call as a video wallpaper. The plugin loads the page in Qt WebEngine. Audio is muted. Pass `--sound` to leave it on. Installing the web plugin does not remove the video plugin.

`--web-player PATH` skips the Plasma wallpaper for that web item and runs the program with the file URL. The default program for that override is `xdg-open`.

```sh
wallpaper play 1234567890 --web-player /usr/bin/firefox
```

## Scene wallpapers

On KDE Plasma 6, `play` of a scene id installs a wallpaper plugin into `~/.local/share/plasma/wallpapers/linux.wallpaper.scene` (or `$XDG_DATA_HOME/plasma/wallpapers/linux.wallpaper.scene`) and selects it with the same Plasma Shell scripting call as a video wallpaper. Image layers in a scene can show on the desktop. The first resolved image or video fills the desktop. A video used as a layer texture loops. Audio for that video is muted. Pass `--sound` to leave it on. A still image has no audio.

Particles, text, models, and scripts still cannot. A scene with no resolvable image or video layer exits with an error and starts no player. Installing the scene plugin does not remove the video or web plugin.

`--video-player` and `--web-player` do not change scene playback. They do not start another program for that item.

```sh
wallpaper play 1234567890
wallpaper play 1234567890 --sound
```

## License

MIT
