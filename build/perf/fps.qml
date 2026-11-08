import QtQuick
import QtQuick.Window
Window {
    width: 160; height: 60; visible: true; title: "fpscounter"
    color: "white"
    property int frames: 0
    property double t0: Date.now()
    Rectangle { width: 20; height: 20; color: "red"; y: 20; x: (frames * 3) % 140 }
    Text { id: label; x: 4; y: 2; text: "fps" }
    FrameAnimation {
        running: true
        onTriggered: {
            frames++;
            const t = Date.now();
            if (t - t0 >= 2000) {
                const f = frames * 1000 / (t - t0);
                console.log("FPS", f.toFixed(1));
                label.text = f.toFixed(1) + " fps";
                frames = 0; t0 = t;
            }
        }
    }
}
