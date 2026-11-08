import QtQuick
import QtQuick.Window
import org.kde.kirigami as Kirigami
Window {
  width: 640; height: 480; visible: true; color: "white"
  Row { spacing: 20; x: 20; y: 20
    Kirigami.ShadowedRectangle { width: 80; height: 80; radius: 40; color: "steelblue" }
    Kirigami.ShadowedRectangle { width: 80; height: 80; radius: 40; color: "steelblue"
      border.width: 2; border.color: "red" }
    Kirigami.ShadowedRectangle { width: 80; height: 80; radius: 40; color: "steelblue"
      shadow.size: 10; shadow.color: "#80000000" }
    Kirigami.ShadowedTexture { width: 80; height: 80; radius: 40; color: "transparent"
      source: Image { visible: false; source: "file:///usr/share/icons/breeze/apps/48/preferences-system.svg"; sourceSize: Qt.size(80,80) } }
    Kirigami.ShadowedTexture { width: 80; height: 80; radius: 40; color: "transparent"
      border.width: 2; border.color: "red"
      source: Image { visible: false; source: "file:///usr/share/icons/breeze/apps/48/preferences-system.svg"; sourceSize: Qt.size(80,80) } }
  }
  Row { spacing: 20; x: 20; y: 140
    Kirigami.Icon { width: 80; height: 80; source: "user-identity" }
    Rectangle { width: 80; height: 80; radius: 40; color: "transparent"; border.width: 1; border.color: "red" }
    Rectangle { width: 80; height: 80; radius: 40; antialiasing: true; color: "steelblue"; border.width: 2; border.color: "red" }
  }
  Row { spacing: 20; x: 20; y: 260
    Rectangle { width: 48; height: 48; radius: 48; border.width: 1.25; border.color: "#e93a9a"; color: "#f0eef0" }
    Rectangle { width: 80; height: 80; radius: 80; border.width: 1.25; border.color: "red"; color: "#f0eef0" }
    Rectangle { width: 80; height: 80; radius: 80; antialiasing: false; border.width: 1.25; border.color: "red"; color: "#f0eef0" }
    Rectangle { width: 80; height: 80; radius: 10; border.width: 1; border.color: "red"; color: "#f0eef0" }
    Rectangle { width: 80; height: 80; rotation: 20; antialiasing: true; border.width: 1; border.color: "red"; color: "#f0eef0" }
  }
}
