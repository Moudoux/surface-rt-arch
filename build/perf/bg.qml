import QtQuick
import QtQuick.Window
Window {
    width: 1366; height: 768; visible: true; title: "bgwin"
    Image { anchors.fill: parent; source: "file:///usr/share/wallpapers/Next/contents/images/5120x2880.png"; sourceSize: Qt.size(1366, 768); fillMode: Image.PreserveAspectCrop }
}
