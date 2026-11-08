// KWin declarative script: moves the first window whose caption contains CAPTION in a circle.
// deploy.sh makes variants (dir/Main.qml each: KWin's QML engine caches by URL and wants a capitalised name).
import QtQuick
import org.kde.kwin
Item {
    id: root
    property var win: null
    property int t: 0
    Timer {
        interval: 16; repeat: true; running: true
        onTriggered: {
            if (!root.win) {
                const ws = Workspace.stackingOrder;
                for (let i = 0; i < ws.length; i++)
                    if (String(ws[i].caption).indexOf("CAPTION") >= 0) root.win = ws[i];
                if (!root.win) return;
            }
            root.t++;
            root.win.frameGeometry = Qt.rect(X0 + RX * Math.sin(root.t / 15), Y0 + RY * Math.cos(root.t / 15), W, H);
        }
    }
}
