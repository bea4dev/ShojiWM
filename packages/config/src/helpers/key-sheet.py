#!/usr/bin/env python3
"""The keyboard cheat sheet (Super+/): every key of the session on one screen.

The config keeps every key in one table (../keymap.ts) and writes it to
$XDG_RUNTIME_DIR/shojiwm-keys.json. Super+/ broadcasts "keys.sheet" on the IPC
socket, and this helper shows or hides the sheet:

    Super+/ ──> config ──"keys.sheet" {toggle}──> this helper
                                                    │ reads shojiwm-keys.json
                         layer-shell surface over everything, keys as keycaps

Any key, a click, or Super+/ again closes it.

Usage: key-sheet.py [IPC socket path]
"""

import json
import math
import os
import sys

from PyQt6.QtCore import QObject, QTimer, QUrl, pyqtProperty, pyqtSignal, pyqtSlot
from PyQt6.QtGui import QGuiApplication
from PyQt6.QtNetwork import QLocalSocket
from PyQt6.QtQml import QQmlApplicationEngine


def runtime_dir() -> str:
    return os.environ.get("XDG_RUNTIME_DIR", "/tmp")


def default_socket_path() -> str:
    """The same path as `defaultSocketPath()` in shoji_wm/ipc."""
    display = os.environ.get("WAYLAND_DISPLAY", "wayland-0")
    return f"{runtime_dir()}/shojiwm-{display}.sock"


SOCKET_PATH = sys.argv[1] if len(sys.argv) > 1 else default_socket_path()
KEYS_FILE = f"{runtime_dir()}/shojiwm-keys.json"

# The groups the sheet shows, in this order. "Debug" stays in `shoji-keys`.
SHOWN_GROUPS = [
    "Apps",
    "Windows",
    "Tiling",
    "Floating",
    "Workspaces",
    "Switchers",
    "Screenshots",
    "Session",
]
GROUP_NOTES = {
    "Tiling": "When the workspace tiles (Super+S)",
    "Floating": "When the workspace floats (Super+S)",
}
# Key names as the keycaps show them.
CAP_NAMES = {
    "Left": "←",
    "Right": "→",
    "Up": "↑",
    "Down": "↓",
    "Arrows": "← ↑ → ↓",
    "Slash": "/",
    "Comma": ",",
    "Period": ".",
    "Print": "PrtSc",
    "Delete": "Del",
    "Wheel": "Wheel",
}
COLUMN_WIDTH = 540


def keycaps(keys: str) -> list[list[str]]:
    """'Super+Shift+Left / Right' -> [['Super', 'Shift', '←'], ['→']]."""
    chords = []
    for chord in keys.split(" / "):
        chords.append([CAP_NAMES.get(part, part) for part in chord.split("+") if part])
    return chords


def load_groups() -> list[dict]:
    try:
        with open(KEYS_FILE) as file:
            data = json.load(file)
    except (OSError, ValueError):
        return []
    groups = []
    for name in SHOWN_GROUPS:
        rows = [
            {"chords": keycaps(row["keys"]), "description": row["description"]}
            for row in data.get("rows", [])
            if row.get("group") == name
        ]
        if rows:
            groups.append({"title": name, "note": GROUP_NOTES.get(name, ""), "rows": rows})
    return groups


def split_columns(groups: list[dict], count: int) -> list[list[dict]]:
    """Fill `count` columns in order, each about the same height."""
    weight = [len(group["rows"]) + 2 for group in groups]
    target = sum(weight) / max(count, 1)
    columns: list[list[dict]] = [[]]
    height = 0
    for group, size in zip(groups, weight):
        if columns[-1] and height + size / 2 > target and len(columns) < count:
            columns.append([])
            height = 0
        columns[-1].append(group)
        height += size
    return columns


QML = """
import QtQuick
import QtQuick.Window
import org.kde.layershell 1.0 as LayerShell

Window {
    id: root
    color: "transparent"
    flags: Qt.FramelessWindowHint
    visible: sheet.shown

    LayerShell.Window.layer: LayerShell.Window.LayerOverlay
    LayerShell.Window.anchors: LayerShell.Window.AnchorTop | LayerShell.Window.AnchorBottom
        | LayerShell.Window.AnchorLeft | LayerShell.Window.AnchorRight
    LayerShell.Window.exclusionZone: -1
    LayerShell.Window.keyboardInteractivity: LayerShell.Window.KeyboardInteractivityExclusive
    LayerShell.Window.scope: "shoji-key-sheet"

    onVisibleChanged: if (visible) { root.requestActivate(); keys.forceActiveFocus(); fade.opacity = 1 } else { fade.opacity = 0 }

    Item {
        id: keys
        anchors.fill: parent
        focus: true
        Keys.onPressed: (event) => { event.accepted = true; sheet.hide() }
    }

    MouseArea {
        anchors.fill: parent
        acceptedButtons: Qt.AllButtons
        onPressed: sheet.hide()
    }

    Item {
        id: fade
        anchors.fill: parent
        opacity: 0
        Behavior on opacity { NumberAnimation { duration: 150; easing.type: Easing.OutCubic } }

        // The desktop dims behind the sheet.
        Rectangle { anchors.fill: parent; color: "#8c000000" }

        Rectangle {
            id: panel
            anchors.centerIn: parent
            width: content.width + 2 * 40
            height: content.height + 2 * 36
            scale: Math.min(1, (root.width - 48) / width, (root.height - 48) / height)
            radius: 20
            color: "#e6202226"
            border.color: "#24ffffff"
            border.width: 1

            Column {
                id: content
                anchors.centerIn: parent
                spacing: 28

                Column {
                    spacing: 6
                    Text {
                        text: "Keyboard shortcuts"
                        color: "#fcfcfc"
                        font.pixelSize: 26
                        font.weight: Font.DemiBold
                    }
                    Text {
                        text: "Press any key or click to close. Run shoji-keys in a terminal for the same list."
                        color: "#99fcfcfc"
                        font.pixelSize: 14
                    }
                }

                Row {
                    spacing: 36
                    Repeater {
                        model: sheet.columns
                        delegate: Column {
                            required property var modelData
                            width: sheet.columnWidth
                            spacing: 26
                            Repeater {
                                model: modelData
                                delegate: Column {
                                    required property var modelData
                                    width: sheet.columnWidth
                                    spacing: 9
                                    Row {
                                        spacing: 10
                                        Text {
                                            text: modelData.title
                                            color: "#3daee9"
                                            font.pixelSize: 15
                                            font.weight: Font.Bold
                                            font.capitalization: Font.AllUppercase
                                            font.letterSpacing: 1.2
                                        }
                                        Text {
                                            text: modelData.note
                                            visible: text.length > 0
                                            color: "#80fcfcfc"
                                            font.pixelSize: 12
                                            topPadding: 3
                                        }
                                    }
                                    Repeater {
                                        model: modelData.rows
                                        delegate: Item {
                                            required property var modelData
                                            width: sheet.columnWidth
                                            height: Math.max(caps.height, label.implicitHeight)
                                            Flow {
                                                id: caps
                                                width: sheet.columnWidth * 0.5
                                                spacing: 4
                                                Repeater {
                                                    model: modelData.chords
                                                    delegate: Row {
                                                        required property var modelData
                                                        required property int index
                                                        spacing: 4
                                                        Text {
                                                            visible: index > 0
                                                            text: "/"
                                                            color: "#66fcfcfc"
                                                            font.pixelSize: 13
                                                            anchors.verticalCenter: parent.verticalCenter
                                                        }
                                                        Repeater {
                                                            model: modelData
                                                            delegate: Rectangle {
                                                                required property var modelData
                                                                height: 26
                                                                width: Math.max(26, capText.implicitWidth + 16)
                                                                radius: 6
                                                                color: "#1affffff"
                                                                border.color: "#33ffffff"
                                                                border.width: 1
                                                                Rectangle {
                                                                    // The keycap's lower edge, like a real key.
                                                                    anchors.left: parent.left
                                                                    anchors.right: parent.right
                                                                    anchors.bottom: parent.bottom
                                                                    anchors.margins: 1
                                                                    height: 2
                                                                    radius: 1
                                                                    color: "#26000000"
                                                                }
                                                                Text {
                                                                    id: capText
                                                                    anchors.centerIn: parent
                                                                    text: modelData
                                                                    color: "#fcfcfc"
                                                                    font.pixelSize: 13
                                                                    font.weight: Font.Medium
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            Text {
                                                id: label
                                                x: sheet.columnWidth * 0.5 + 12
                                                width: sheet.columnWidth * 0.5 - 12
                                                anchors.verticalCenter: parent.verticalCenter
                                                text: modelData.description
                                                color: "#dcfcfcfc"
                                                font.pixelSize: 14
                                                wrapMode: Text.WordWrap
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
"""


class Sheet(QObject):
    changed = pyqtSignal()

    def __init__(self, screen_width: int):
        super().__init__()
        self._shown = False
        self._columns: list = []
        self.screen_width = screen_width

    @pyqtProperty(bool, notify=changed)
    def shown(self):
        return self._shown

    @pyqtProperty("QVariantList", notify=changed)
    def columns(self):
        return self._columns

    @pyqtProperty(int, constant=True)
    def columnWidth(self):
        return COLUMN_WIDTH

    def show(self):
        groups = load_groups()
        if not groups:
            return
        count = max(1, min(4, math.floor((self.screen_width - 160) / (COLUMN_WIDTH + 36))))
        self._columns = split_columns(groups, count)
        self._shown = True
        self.changed.emit()

    @pyqtSlot()
    def hide(self):
        if self._shown:
            self._shown = False
            self.changed.emit()

    def toggle(self):
        if self._shown:
            self.hide()
        else:
            self.show()


class Connection(QObject):
    """The IPC socket: reads the config's broadcasts, reconnects when it drops."""

    def __init__(self, sheet: Sheet):
        super().__init__()
        self.sheet = sheet
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
        self.sheet.hide()
        QTimer.singleShot(1000, self.connect_now)

    def read(self):
        self.buffer += bytes(self.socket.readAll())
        *lines, self.buffer = self.buffer.split(b"\n")
        for line in lines:
            try:
                message = json.loads(line)
            except ValueError:
                continue
            if message.get("event") == "keys.sheet":
                self.sheet.toggle()


def main():
    os.environ.setdefault("QT_QPA_PLATFORM", "wayland")
    app = QGuiApplication(sys.argv)
    screen = app.primaryScreen()
    sheet = Sheet(screen.size().width() if screen else 1920)
    engine = QQmlApplicationEngine()
    engine.rootContext().setContextProperty("sheet", sheet)
    engine.loadData(QML.encode(), QUrl("key-sheet.qml"))
    if not engine.rootObjects():
        sys.exit("key-sheet: the QML did not load")
    connection = Connection(sheet)  # noqa: F841 (kept alive)
    sys.exit(app.exec())


if __name__ == "__main__":
    main()
