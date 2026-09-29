import QtQuick
import QtWebEngine
import org.kde.plasma.plasmoid

WallpaperItem {
    id: root

    // A wallpaper that starts with sound is the wrong default. The shell
    // script also writes Muted; this binding keeps later config edits live.
    // Muted true (the default, and an unset value) mutes the page. Muted
    // false leaves the page audio on.
    WebEngineView {
        id: page
        anchors.fill: parent
        url: root.configuration.PageUrl || ""
        audioMuted: root.configuration.Muted !== false
        backgroundColor: "black"

        settings.playbackRequiresUserGesture: false

        onLoadingChanged: function(loadRequest) {
            if (loadRequest.errorString) {
                console.warn("linux.wallpaper.web:", loadRequest.errorString)
            }
        }
    }
}
