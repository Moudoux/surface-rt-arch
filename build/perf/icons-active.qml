// icons.qml: 120 Kirigami icons (64 px) redrawn every frame (the grid slides),
// logs its own FPS ("ICONFPS"). ACTIVE true = highlight path (full icon shader).
import QtQuick
import QtQuick.Window
import org.kde.kirigami as Kirigami
Window {
    width: 820; height: 520; visible: true; title: "iconbench"
    color: "#202020"
    property int frames: 0
    property double t0: Date.now()
    readonly property var names: ["folder", "document-open", "edit-copy", "edit-paste", "go-home",
        "system-search", "preferences-system", "utilities-terminal", "applications-internet",
        "multimedia-player", "help-about", "user-trash"]
    Grid {
        x: 10 + (frames % 40); y: 10
        columns: 12; spacing: 2
        Repeater {
            model: 84
            Kirigami.Icon {
                width: 64; height: 64
                source: names[index % names.length]
                active: true
            }
        }
    }
    FrameAnimation {
        running: true
        onTriggered: {
            frames++;
            const t = Date.now();
            if (t - t0 >= 2000) {
                console.log("ICONFPS", (frames * 1000 / (t - t0)).toFixed(1));
                frames = 0; t0 = t;
            }
        }
    }
}
