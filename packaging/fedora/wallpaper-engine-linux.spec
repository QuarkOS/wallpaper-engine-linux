%global debug_package %{nil}

Name:           wallpaper-engine-linux
Version:        0.1.0
Release:        1%{?dist}
Summary:        Play owned Wallpaper Engine items as KDE Plasma wallpapers
License:        MIT
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  gcc
BuildRequires:  pkgconf-pkg-config
BuildRequires:  gtk3-devel
BuildRequires:  webkit2gtk4.1-devel
BuildRequires:  curl
BuildRequires:  ca-certificates
BuildRequires:  rust
BuildRequires:  cargo
# %%build uses the system toolchain when rustc is at least 1.98.1, the
# version in rust-toolchain.toml. An older Fedora cargo installs that
# toolchain with rustup instead of failing the build.

Requires:       gtk3
Requires:       webkit2gtk4.1
Requires:       qt6-qttools
Requires:       qt6-qtmultimedia
Requires:       qt6-qtwebengine

%description
wallpaper plays video, web, and scene Wallpaper Engine workshop items that
you already own. It reads the files Steam downloaded. wallpaper-desktop opens
that library in a window. The monochrome stylesheet is installed under
/usr/share/wallpaper-engine-linux. This package is not Wallpaper Engine.

%prep
%autosetup -n %{name}-%{version}

%build
set -eu
required=1.98.1
use_rustup=0
if ! command -v rustc >/dev/null 2>&1 || ! command -v cargo >/dev/null 2>&1; then
  use_rustup=1
else
  have=$(rustc --version | awk '{print $2}')
  oldest=$(printf '%s\n%s\n' "$required" "$have" | sort -V | head -n1)
  if [ "$oldest" != "$required" ]; then
    use_rustup=1
  fi
fi
if [ "$use_rustup" -eq 1 ]; then
  export RUSTUP_HOME="$PWD/rustup"
  export CARGO_HOME="$PWD/cargo"
  curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs -o rustup-init.sh
  sh rustup-init.sh -y --default-toolchain "$required" --profile minimal --no-modify-path
  export PATH="$CARGO_HOME/bin:$PATH"
fi
cargo build --release --locked --bin wallpaper --bin wallpaper-desktop

%install
install -D -m 0755 target/release/wallpaper %{buildroot}%{_bindir}/wallpaper
install -D -m 0755 target/release/wallpaper-desktop %{buildroot}%{_bindir}/wallpaper-desktop
install -D -m 0644 ui/styles.css %{buildroot}%{_datadir}/wallpaper-engine-linux/ui/styles.css
install -D -m 0644 packaging/fedora/wallpaper-engine-linux.desktop %{buildroot}%{_datadir}/applications/wallpaper-engine-linux.desktop

%files
%{_bindir}/wallpaper
%{_bindir}/wallpaper-desktop
%{_datadir}/wallpaper-engine-linux/
%{_datadir}/applications/wallpaper-engine-linux.desktop

%changelog
* Tue Sep 29 2026 Wallpaper Engine Linux <wallpaper-engine-linux@localhost> - 0.1.0-1
- Package wallpaper and wallpaper-desktop for Fedora.
