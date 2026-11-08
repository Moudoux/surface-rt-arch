// rtcheck.qml: static scene with many draws per frame (rects, text, opacity,
// rotation, clipping) for pixel A/B between Mesa builds.
import QtQuick
import QtQuick.Window
Window {
    width: 820; height: 520; visible: true; title: "rtcheck"
    color: "#203040"
    Grid {
        x: 8; y: 8; columns: 16; spacing: 3
        Repeater {
            model: 128
            Rectangle {
                width: 46; height: 46; radius: index % 3 ? 8 : 0
                color: Qt.hsla((index * 37 % 360) / 360, 0.7, 0.5, index % 5 ? 1 : 0.6)
                border.width: index % 4 ? 0 : 2; border.color: "white"
                rotation: index % 7 ? 0 : 15
                Text { anchors.centerIn: parent; text: index; color: "black"; font.pixelSize: 14 }
            }
        }
    }
    Rectangle {
        x: 40; y: 420; width: 740; height: 80; clip: true
        gradient: Gradient { GradientStop { position: 0; color: "orange" } GradientStop { position: 1; color: "purple" } }
        Text { x: 10; y: 10; text: "Render target emitted once per job"; font.pixelSize: 40; color: "white" }
    }
}
