import QtQuick
import QtQuick.Controls as QtControls
import org.kde.kirigami as Kirigami

Kirigami.FormLayout {
    id: root

    property alias cfg_VideoFile: fileField.text
    property alias cfg_Muted: muteBox.checked

    QtControls.TextField {
        id: fileField
        Kirigami.FormData.label: "Video file:"
        placeholderText: "file:///path/to/video.mp4"
    }

    QtControls.CheckBox {
        id: muteBox
        Kirigami.FormData.label: "Audio:"
        text: "Mute"
    }
}
