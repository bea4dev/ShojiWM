#!/usr/bin/env python3
"""KWin's screenshot D-Bus service, so Spectacle works under ShojiWM.

On Wayland, Spectacle can only take screenshots through KWin's
org.kde.KWin.ScreenShot2 service. It has no portal fallback, so without KWin
it quits at once. This service owns that name and answers the same calls:
it grabs the screen with grim, then writes the raw pixels into the pipe that
Spectacle passes, the way KWin does.

    Spectacle ──CaptureWorkspace(options, pipe)──> this service
                                                   │ grim -t ppm -
    Spectacle <──results {width, height, format}── │
    Spectacle <──raw RGBX pixels through the pipe──┘

The config must answer two IPC requests (see `createIpcServer`):

- `workspaces.get`: `{ currentMonitor, monitors: [{ name, workspaces:
  [{ active, windows: [{ id, focused }] }] }] }`
- `windows.rect` with `{ windowId }`: `{ x, y, width, height }` in global
  logical px, or `null`

Usage: screenshot-dbus.py [IPC socket path]
"""

import json
import os
import shutil
import socket
import subprocess
import sys
import threading

from gi.repository import Gio, GLib

BUS_NAME = "org.kde.KWin.ScreenShot2"
OBJECT_PATH = "/org/kde/KWin/ScreenShot2"
# The interface version Spectacle reads. 4 adds the windowId and screen keys.
VERSION = 4
# QImage::Format_RGBX8888: 4 bytes per pixel, R G B and an unused byte.
QIMAGE_FORMAT_RGBX8888 = 16

INTERFACE_XML = """
<node>
  <interface name="org.kde.KWin.ScreenShot2">
    <property name="Version" type="u" access="read"/>
    <method name="CaptureWindow">
      <arg name="handle" type="s" direction="in"/>
      <arg name="options" type="a{sv}" direction="in"/>
      <arg name="pipe" type="h" direction="in"/>
      <arg name="results" type="a{sv}" direction="out"/>
    </method>
    <method name="CaptureActiveWindow">
      <arg name="options" type="a{sv}" direction="in"/>
      <arg name="pipe" type="h" direction="in"/>
      <arg name="results" type="a{sv}" direction="out"/>
    </method>
    <method name="CaptureArea">
      <arg name="x" type="i" direction="in"/>
      <arg name="y" type="i" direction="in"/>
      <arg name="width" type="u" direction="in"/>
      <arg name="height" type="u" direction="in"/>
      <arg name="options" type="a{sv}" direction="in"/>
      <arg name="pipe" type="h" direction="in"/>
      <arg name="results" type="a{sv}" direction="out"/>
    </method>
    <method name="CaptureScreen">
      <arg name="name" type="s" direction="in"/>
      <arg name="options" type="a{sv}" direction="in"/>
      <arg name="pipe" type="h" direction="in"/>
      <arg name="results" type="a{sv}" direction="out"/>
    </method>
    <method name="CaptureActiveScreen">
      <arg name="options" type="a{sv}" direction="in"/>
      <arg name="pipe" type="h" direction="in"/>
      <arg name="results" type="a{sv}" direction="out"/>
    </method>
    <method name="CaptureInteractive">
      <arg name="kind" type="u" direction="in"/>
      <arg name="options" type="a{sv}" direction="in"/>
      <arg name="pipe" type="h" direction="in"/>
      <arg name="results" type="a{sv}" direction="out"/>
    </method>
    <method name="CaptureWorkspace">
      <arg name="options" type="a{sv}" direction="in"/>
      <arg name="pipe" type="h" direction="in"/>
      <arg name="results" type="a{sv}" direction="out"/>
    </method>
  </interface>
</node>
"""


def default_socket_path() -> str:
    """The same path as `defaultSocketPath()` in shoji_wm/ipc."""
    runtime_dir = os.environ.get("XDG_RUNTIME_DIR", "/tmp")
    display = os.environ.get("WAYLAND_DISPLAY", "wayland-0")
    return f"{runtime_dir}/shojiwm-{display}.sock"


SOCKET_PATH = sys.argv[1] if len(sys.argv) > 1 else default_socket_path()


def find_grim() -> tuple[str, dict[str, str]]:
    """grim from PATH."""
    found = shutil.which("grim")
    if not found:
        raise RuntimeError("grim is not installed")
    return found, dict(os.environ)


def ask_config(method: str, params: dict | None = None):
    """Send one IPC request and return its result (None on any failure)."""
    message = {"id": 1, "method": method, "params": params or {}}
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
            connection.settimeout(2)
            connection.connect(SOCKET_PATH)
            connection.sendall((json.dumps(message) + "\n").encode())
            reply = b""
            while not reply.endswith(b"\n"):
                chunk = connection.recv(65536)
                if not chunk:
                    break
                reply += chunk
        return json.loads(reply).get("result")
    except (OSError, ValueError) as error:
        print(f"screenshot-dbus: IPC {method} failed: {error}", file=sys.stderr)
        return None


def current_monitor() -> str | None:
    view = ask_config("workspaces.get")
    return view.get("currentMonitor") if isinstance(view, dict) else None


def focused_window_id() -> str | None:
    """The focused window on the active workspace of the current monitor."""
    view = ask_config("workspaces.get")
    if not isinstance(view, dict):
        return None
    for monitor in view.get("monitors", []):
        if monitor.get("name") != view.get("currentMonitor"):
            continue
        for workspace in monitor.get("workspaces", []):
            if not workspace.get("active"):
                continue
            for window in workspace.get("windows", []):
                if window.get("focused"):
                    return window.get("id")
    return None


def run_grim(grim_args: list[str]) -> bytes:
    """Run grim with `grim_args` and return the PPM it writes."""
    grim, env = find_grim()
    command = [grim, "-t", "ppm"] + grim_args + ["-"]
    return subprocess.run(command, env=env, capture_output=True, check=True).stdout


def parse_ppm(ppm: bytes) -> tuple[int, int, int]:
    """Return (width, height, offset of the pixel data) of a P6 PPM."""
    fields = []
    position = 0
    while len(fields) < 4:
        while ppm[position : position + 1].isspace():
            position += 1
        if ppm[position : position + 1] == b"#":
            position = ppm.index(b"\n", position) + 1
            continue
        end = position
        while not ppm[end : end + 1].isspace():
            end += 1
        fields.append(ppm[position:end])
        position = end
    return int(fields[1]), int(fields[2]), position + 1


def grim_capture(grim_args: list[str], include_cursor: bool) -> tuple[int, int, bytes]:
    """Run grim and return (width, height, RGBX pixels)."""
    ppm = run_grim((["-c"] if include_cursor else []) + grim_args)
    # PPM (P6) header: "P6", width, height, max value, then RGB bytes.
    width, height, offset = parse_ppm(ppm)
    rgb = ppm[offset : offset + width * height * 3]
    # RGB to RGBX: copy each channel into every fourth byte.
    rgbx = bytearray(b"\xff" * (width * height * 4))
    rgbx[0::4] = rgb[0::3]
    rgbx[1::4] = rgb[1::3]
    rgbx[2::4] = rgb[2::3]
    return width, height, bytes(rgbx)


def logical_width(grim_args: list[str]) -> int:
    """The width of the captured region in logical px.

    grim renders at the greatest output scale, so on a fractional scale the
    image is wider than the region. `-s 1` asks grim for logical px instead;
    only the header of that second image is read.
    """
    width, _height, _offset = parse_ppm(run_grim(["-s", "1"] + grim_args))
    return width


def write_pipe(fd: int, data: bytes) -> None:
    """Write the pixels after the reply, as KWin does, then close the pipe."""
    try:
        with os.fdopen(fd, "wb") as pipe:
            pipe.write(data)
    except OSError as error:
        print(f"screenshot-dbus: pipe write failed: {error}", file=sys.stderr)


def capture(method: str, args: tuple, options: dict) -> tuple[list[str], dict, int | None]:
    """Map a ScreenShot2 call to grim arguments, extra result keys and the
    region's logical width when the call already says it."""
    if method == "CaptureWorkspace":
        return [], {}, None
    if method == "CaptureArea":
        x, y, width, height = args[:4]
        return ["-g", f"{x},{y} {width}x{height}"], {}, width
    if method == "CaptureScreen":
        return ["-o", args[0]], {"screen": GLib.Variant("s", args[0])}, None
    if method in ("CaptureWindow", "CaptureActiveWindow") or (
        method == "CaptureInteractive" and args[0] == 0
    ):
        # Kind 0 of CaptureInteractive is "pick a window". There is no picker
        # here, so it takes the focused window, like CaptureActiveWindow.
        window_id = args[0] if method == "CaptureWindow" else focused_window_id()
        rect = ask_config("windows.rect", {"windowId": window_id}) if window_id else None
        if isinstance(rect, dict):
            width = round(rect["width"])
            geometry = f"{round(rect['x'])},{round(rect['y'])} {width}x{round(rect['height'])}"
            return ["-g", geometry], {"windowId": GLib.Variant("s", window_id)}, width
    # Active screen, interactive screen pick, or a window we could not find.
    monitor = current_monitor()
    if monitor:
        return ["-o", monitor], {"screen": GLib.Variant("s", monitor)}, None
    return [], {}, None


def on_method_call(connection, _sender, _path, _interface, method, parameters, invocation):
    args = parameters.unpack()
    # The pipe is the last "h" argument; its value indexes the passed fd list.
    fd_list = invocation.get_message().get_unix_fd_list()
    pipe_index = args[-1]
    options = args[-2]
    fd = None
    try:
        fd = fd_list.get(pipe_index)
        grim_args, extra, region_width = capture(method, args[:-2], options)
        width, height, pixels = grim_capture(grim_args, bool(options.get("include-cursor", False)))
        # The image's device pixel ratio, as KWin reports it: Spectacle maps
        # logical coordinates (its region selection) onto the image with it.
        scale = width / (region_width or logical_width(grim_args) or width)
        # Logical sizes are whole px, so the ratio is a little off; output
        # scales are multiples of 1/120 (wp_fractional_scale_v1).
        scale = round(scale * 120) / 120
    except Exception as error:  # noqa: BLE001 - any failure goes back to Spectacle
        if fd is not None:
            # Close our copy of the pipe so Spectacle's read ends.
            os.close(fd)
        invocation.return_dbus_error("org.kde.KWin.ScreenShot2.Error.Cancelled", str(error))
        return
    results = {
        "type": GLib.Variant("s", "raw"),
        "width": GLib.Variant("u", width),
        "height": GLib.Variant("u", height),
        "stride": GLib.Variant("u", width * 4),
        "format": GLib.Variant("u", QIMAGE_FORMAT_RGBX8888),
        "scale": GLib.Variant("d", scale),
        **extra,
    }
    invocation.return_value(GLib.Variant("(a{sv})", (results,)))
    threading.Thread(target=write_pipe, args=(fd, pixels), daemon=True).start()


def on_get_property(_connection, _sender, _path, _interface, name):
    if name == "Version":
        return GLib.Variant("u", VERSION)
    return None


def on_bus_acquired(connection, _name):
    # Register the object before the name shows up, so Spectacle never sees
    # the name without the object.
    interface = Gio.DBusNodeInfo.new_for_xml(INTERFACE_XML).interfaces[0]
    connection.register_object(OBJECT_PATH, interface, on_method_call, on_get_property, None)


def on_name_lost(_connection, _name):
    print(f"screenshot-dbus: {BUS_NAME} is taken (is KWin running?)", file=sys.stderr)
    sys.exit(1)


Gio.bus_own_name(
    Gio.BusType.SESSION,
    BUS_NAME,
    Gio.BusNameOwnerFlags.NONE,
    on_bus_acquired,
    None,
    on_name_lost,
)
GLib.MainLoop().run()
