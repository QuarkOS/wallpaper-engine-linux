import QtQuick
import QtMultimedia
import org.kde.plasma.plasmoid

WallpaperItem {
    id: root

    // A wallpaper that starts with sound is the wrong default. The shell
    // script also writes Muted; this binding keeps later config edits live.
    MediaPlayer {
        id: player
        source: root.configuration.VideoFile || ""
        loops: MediaPlayer.Infinite
        audioOutput: audio
        videoOutput: output

        onSourceChanged: {
            if (source != "") {
                play()
            }
        }

        onMediaStatusChanged: {
            if (mediaStatus === MediaPlayer.EndOfMedia) {
                play()
            }
        }

        onErrorOccurred: function(error, errorString) {
            console.warn("linux.wallpaper.video:", errorString)
        }
    }

    AudioOutput {
        id: audio
        muted: root.configuration.Muted !== false
        volume: muted ? 0 : 1
    }

    VideoOutput {
        id: output
        anchors.fill: parent
        fillMode: VideoOutput.PreserveAspectCrop
    }

    Component.onCompleted: {
        if (player.source != "") {
            player.play()
        }
    }
}
