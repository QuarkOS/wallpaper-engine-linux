import QtQuick
import QtQuick.Controls as QtControls
import org.kde.kirigami as Kirigami

Kirigami.FormLayout {
    id: root

    property alias cfg_PageUrl: pageField.text
    property alias cfg_Muted: muteBox.checked

    QtControls.TextField {
        id: pageField
        Kirigami.FormData.label: "Page:"
        placeholderText: "file:///path/to/index.html"
    }

    QtControls.CheckBox {
        id: muteBox
        Kirigami.FormData.label: "Audio:"
        text: "Mute"
    }
}
