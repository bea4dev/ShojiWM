#!/usr/bin/env python3
"""The Coverflow switcher's title: the selected window's icon and title.

ShojiWM's output composition cannot draw text, so this helper draws it. It is
a layer-shell surface on the Overlay layer, which the switcher's composition
draws over the cards. The config broadcasts the selected window on its IPC
socket, and this helper shows it:

    config ──"switcher.label" {visible, title, appId}──> this helper
                                                          │
                     layer-shell surface, bottom quarter of the screen

The look and place follow the GNOME extension Coverflow Alt-Tab with its
defaults: icon style "Classic" (a 64 px icon, 10 px left of the title), the
title box centred 7/8 down the screen, bold text with 1em of padding, and a
fade of 0.2 s.

Usage: switcher-label.py [IPC socket path]
"""

import configparser
import json
import os
import sys

from PyQt6.QtCore import QObject, QTimer, QUrl, pyqtProperty, pyqtSignal
from PyQt6.QtGui import QGuiApplication, QIcon
from PyQt6.QtNetwork import QLocalSocket
from PyQt6.QtQml import QQmlApplicationEngine
from PyQt6.QtQuick import QQuickImageProvider


def default_socket_path() -> str:
    """The same path as `defaultSocketPath()` in shoji_wm/ipc."""
    runtime_dir = os.environ.get("XDG_RUNTIME_DIR", "/tmp")
    display = os.environ.get("WAYLAND_DISPLAY", "wayland-0")
    return f"{runtime_dir}/shojiwm-{display}.sock"


SOCKET_PATH = sys.argv[1] if len(sys.argv) > 1 else default_socket_path()

QML = """
import QtQuick
import QtQuick.Window
import org.kde.layershell 1.0 as LayerShell

Window {
    id: root
    // The bottom quarter of the screen: its middle is 7/8 down the screen.
    width: Screen.width
    height: Math.round(Screen.height / 4)
    color: "transparent"
    flags: Qt.FramelessWindowHint | Qt.WindowTransparentForInput
    visible: label.shown || group.opacity > 0

    LayerShell.Window.layer: LayerShell.Window.LayerOverlay
    LayerShell.Window.anchors: LayerShell.Window.AnchorBottom
        | LayerShell.Window.AnchorLeft | LayerShell.Window.AnchorRight
    LayerShell.Window.exclusionZone: -1
    LayerShell.Window.keyboardInteractivity: LayerShell.Window.KeyboardInteractivityNone
    LayerShell.Window.scope: "shoji-switcher-label"

    Item {
        id: group
        anchors.fill: parent
        opacity: label.shown ? 1 : 0
        Behavior on opacity { NumberAnimation { duration: 200; easing.type: Easing.OutQuad } }

        // ICON_SIZE + ICON_TITLE_SPACING: the title moves right by half of it,
        // so the icon and the title together sit in the middle.
        readonly property int iconSize: 64
        readonly property int spacing: 10

        Rectangle {
            id: box
            x: Math.round((root.width - width + group.iconSize + group.spacing) / 2)
            y: Math.round((root.height - height) / 2)
            width: Math.min(text.implicitWidth + 2 * padding, root.width - 200)
            height: text.implicitHeight + 2 * padding
            radius: 12
            color: "#f22c2d32"
            readonly property real padding: text.font.pixelSize
            Text {
                id: text
                anchors.centerIn: parent
                width: Math.min(implicitWidth, parent.width - 2 * parent.padding)
                text: label.title
                color: "white"
                font.bold: true
                font.pixelSize: 15
                elide: Text.ElideRight
            }
        }

        Image {
            x: box.x - group.iconSize - group.spacing
            y: Math.round((root.height - height) / 2)
            width: group.iconSize
            height: group.iconSize
            sourceSize.width: group.iconSize
            sourceSize.height: group.iconSize
            source: label.icon === "" ? "" : "image://appicon/" + label.icon
        }
    }
}
"""


class IconProvider(QQuickImageProvider):
    """Icons from the icon theme, by name."""

    def __init__(self):
        super().__init__(QQuickImageProvider.ImageType.Pixmap)

    def requestPixmap(self, name, requested):
        icon = QIcon.fromTheme(name)
        if icon.isNull():
            icon = QIcon.fromTheme("application-x-executable")
        side = requested.width() if requested.width() > 0 else 64
        pixmap = icon.pixmap(side, side)
        return pixmap, pixmap.size()


def icon_name(app_id: str) -> str:
    """The Icon= of the app's desktop file, or the app id itself."""
    dirs = [os.path.expanduser("~/.local/share")]
    dirs += os.environ.get("XDG_DATA_DIRS", "/usr/local/share:/usr/share").split(":")
    for data_dir in dirs:
        path = os.path.join(data_dir, "applications", f"{app_id}.desktop")
        if not os.path.exists(path):
            continue
        entry = configparser.ConfigParser(interpolation=None, strict=False)
        try:
            entry.read(path, encoding="utf-8")
            return entry.get("Desktop Entry", "Icon", fallback=app_id)
        except (configparser.Error, UnicodeDecodeError):
            return app_id
    return app_id


def icon_theme() -> str:
    """KDE's icon theme (kdeglobals), else Breeze."""
    entry = configparser.ConfigParser(interpolation=None, strict=False)
    entry.read(os.path.expanduser("~/.config/kdeglobals"), encoding="utf-8")
    return entry.get("Icons", "Theme", fallback="breeze")


class Label(QObject):
    """What the title shows; QML reads it."""

    changed = pyqtSignal()

    def __init__(self):
        super().__init__()
        self._shown = False
        self._title = ""
        self._icon = ""

    @pyqtProperty(bool, notify=changed)
    def shown(self):
        return self._shown

    @pyqtProperty(str, notify=changed)
    def title(self):
        return self._title

    @pyqtProperty(str, notify=changed)
    def icon(self):
        return self._icon

    def update(self, payload: dict):
        self._shown = bool(payload.get("visible"))
        if self._shown:
            # While it fades out, the last title stays.
            self._title = str(payload.get("title") or "")
            app_id = str(payload.get("appId") or "")
            self._icon = icon_name(app_id) if app_id else ""
        self.changed.emit()


class Connection(QObject):
    """The IPC socket: reads the config's broadcasts, reconnects when it drops."""

    def __init__(self, label: Label):
        super().__init__()
        self.label = label
        self.buffer = b""
        self.waiting = False
        self.socket = QLocalSocket()
        self.socket.readyRead.connect(self.read)
        self.socket.disconnected.connect(self.retry)
        self.socket.errorOccurred.connect(lambda _error: self.retry())
        self.connect_now()

    def connect_now(self):
        self.waiting = False
        self.buffer = b""
        self.socket.connectToServer(SOCKET_PATH)

    def retry(self):
        # An error and a disconnect can both come: wait for one retry only.
        if self.waiting:
            return
        self.waiting = True
        self.label.update({"visible": False})
        QTimer.singleShot(1000, self.connect_now)

    def read(self):
        self.buffer += bytes(self.socket.readAll())
        *lines, self.buffer = self.buffer.split(b"\n")
        for line in lines:
            try:
                message = json.loads(line)
            except ValueError:
                continue
            if message.get("event") == "switcher.label":
                self.label.update(message.get("payload") or {})


def main():
    os.environ.setdefault("QT_QPA_PLATFORM", "wayland")
    app = QGuiApplication(sys.argv)
    QIcon.setThemeName(icon_theme())
    label = Label()
    engine = QQmlApplicationEngine()
    engine.addImageProvider("appicon", IconProvider())
    engine.rootContext().setContextProperty("label", label)
    engine.loadData(QML.encode(), QUrl("switcher-label.qml"))
    if not engine.rootObjects():
        sys.exit("switcher-label: the QML did not load")
    connection = Connection(label)  # noqa: F841 (kept alive)
    sys.exit(app.exec())


if __name__ == "__main__":
    main()
