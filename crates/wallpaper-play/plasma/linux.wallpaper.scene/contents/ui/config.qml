import QtQuick
import QtQuick.Controls as QtControls
import org.kde.kirigami as Kirigami

Kirigami.FormLayout {
    id: root

    property alias cfg_Layers: layersField.text
    property alias cfg_Muted: muteBox.checked

    QtControls.TextField {
        id: layersField
        Kirigami.FormData.label: "Layers:"
        placeholderText: "[{\"url\":\"file:///path/to/layer.png\",\"kind\":\"image\"}]"
    }

    QtControls.CheckBox {
        id: muteBox
        Kirigami.FormData.label: "Video audio:"
        text: "Mute"
    }
}
