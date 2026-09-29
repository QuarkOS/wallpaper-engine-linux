import QtQuick
import QtMultimedia
import org.kde.plasma.plasmoid

WallpaperItem {
    id: root

    // Layers is a JSON array of {url, kind} objects written by the shell
    // script. kind is "image" or "video". Each resolved layer is shown.
    // The first resolved visual fills the desktop.
    readonly property string layersConfig: root.configuration.Layers || "[]"
    readonly property var layerItems: {
        try {
            var parsed = JSON.parse(layersConfig)
            if (parsed && parsed.length !== undefined) {
                return parsed
            }
        } catch (error) {
            console.warn("linux.wallpaper.scene:", error)
        }
        return []
    }

    Rectangle {
        anchors.fill: parent
        color: "black"
        z: -1
    }

    Repeater {
        model: root.layerItems

        Item {
            // The first resolved visual fills the desktop. Later layers are
            // shown above it.
            anchors.fill: parent
            z: index

            Image {
                anchors.fill: parent
                visible: modelData.kind !== "video"
                source: modelData.kind === "video" ? "" : (modelData.url || "")
                fillMode: Image.PreserveAspectCrop
                asynchronous: true
                cache: false
            }

            MediaPlayer {
                id: player
                // A video texture loops. A still image leaves this source
                // empty and does not attach audio.
                source: modelData.kind === "video" ? (modelData.url || "") : ""
                loops: MediaPlayer.Infinite
                audioOutput: modelData.kind === "video" ? audio : null
                videoOutput: output

                onSourceChanged: {
                    if (source != "") {
                        play()
                    }
                }

                onMediaStatusChanged: {
                    if (modelData.kind === "video" && mediaStatus === MediaPlayer.EndOfMedia) {
                        play()
                    }
                }

                onErrorOccurred: function(error, errorString) {
                    console.warn("linux.wallpaper.scene:", errorString)
                }
            }

            AudioOutput {
                id: audio
                // Still images have no audio. Video follows Muted, which
                // defaults to true. --sound writes Muted false.
                muted: root.configuration.Muted !== false
                volume: muted ? 0 : 1
            }

            VideoOutput {
                id: output
                anchors.fill: parent
                visible: modelData.kind === "video"
                fillMode: VideoOutput.PreserveAspectCrop
            }

            Component.onCompleted: {
                if (player.source != "") {
                    player.play()
                }
            }
        }
    }
}
