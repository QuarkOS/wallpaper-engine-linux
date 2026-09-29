//! The Fedora spec names the binaries, the installed stylesheet, and the menu entry.

use std::fs;
use std::path::PathBuf;

fn repo_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn installs_binary(spec: &str, bin: &str) -> bool {
    spec.lines().any(|line| {
        let source = format!("target/release/{bin}");
        let dest = format!("%{{_bindir}}/{bin}");
        line.contains(&source)
            && line.contains(&dest)
            && !line.contains(&format!("target/release/{bin}-"))
            && !line.contains(&format!("%{{_bindir}}/{bin}-"))
    })
}

#[test]
fn fedora_spec_names_binaries_stylesheet_and_desktop_file() {
    let spec = repo_file("../../packaging/fedora/wallpaper-engine-linux.spec");
    let desktop = repo_file("../../packaging/fedora/wallpaper-engine-linux.desktop");

    assert!(
        installs_binary(&spec, "wallpaper"),
        "spec should install the wallpaper binary"
    );
    assert!(
        installs_binary(&spec, "wallpaper-desktop"),
        "spec should install the wallpaper-desktop binary"
    );
    assert!(
        spec.contains("%{_bindir}/wallpaper\n") || spec.contains("%{_bindir}/wallpaper\r\n"),
        "spec should package the wallpaper binary"
    );
    assert!(
        spec.contains("%{_bindir}/wallpaper-desktop"),
        "spec should package wallpaper-desktop"
    );
    assert!(
        spec.contains("/usr/share/wallpaper-engine-linux")
            || spec.contains("%{_datadir}/wallpaper-engine-linux/ui/styles.css"),
        "spec should install the stylesheet at the share path"
    );
    assert!(
        spec.contains("wallpaper-engine-linux/ui/styles.css"),
        "spec should install ui/styles.css under the package data directory"
    );
    assert!(
        spec.contains("wallpaper-engine-linux.desktop"),
        "spec should install the desktop file"
    );
    assert!(
        spec.contains("1.98.1"),
        "spec should match the rust-toolchain pin"
    );
    assert!(
        spec.contains("rustup"),
        "spec should use rustup when the system cargo is older"
    );
    assert!(
        desktop.contains("Exec=wallpaper desktop"),
        "desktop entry should launch wallpaper desktop"
    );
}
