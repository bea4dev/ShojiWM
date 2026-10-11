#!/usr/bin/env python3
"""KWin's Window View D-Bus service, for plasmashell's taskbar.

The taskbar setting "Clicking grouped task: Show windows side by side" calls
org.kde.KWin.Effect.WindowView1.activate(as) with the windows' ids. It only
offers that while some program owns the bus name; with no owner it falls
back to the textual list of windows.

This service owns the name and passes the ids on to the ShojiWM config over
its IPC socket (the `windowView.activate` command), which opens the window
view. The ids are the org_kde_plasma_window_management uuids, which are the
ShojiWM window ids.

    taskbar ──activate([ids])──> this service ──IPC──> config: window view

Usage: window-view-dbus.py [IPC socket path]
"""

import json
import os
import socket
import sys

from gi.repository import Gio, GLib

BUS_NAME = "org.kde.KWin.Effect.WindowView1"
OBJECT_PATH = "/org/kde/KWin/Effect/WindowView1"
INTERFACE_XML = """
<node>
  <interface name="org.kde.KWin.Effect.WindowView1">
    <method name="activate">
      <arg name="handles" type="as" direction="in"/>
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


def send_to_config(window_ids: list[str]) -> None:
    """Send one fire-and-forget IPC command (no "id", so no reply)."""
    message = {"method": "windowView.activate", "params": {"windowIds": window_ids}}
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
            connection.connect(SOCKET_PATH)
            connection.sendall((json.dumps(message) + "\n").encode())
    except OSError as error:
        print(f"window-view-dbus: cannot reach {SOCKET_PATH}: {error}", file=sys.stderr)


def on_method_call(_connection, _sender, _path, _interface, method, parameters, invocation):
    if method == "activate":
        (window_ids,) = parameters.unpack()
        send_to_config(list(window_ids))
        invocation.return_value(None)
    else:
        invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method)


def on_bus_acquired(connection, _name):
    # Register the object before the name shows up, so the taskbar never sees
    # the name without the object.
    interface = Gio.DBusNodeInfo.new_for_xml(INTERFACE_XML).interfaces[0]
    connection.register_object(OBJECT_PATH, interface, on_method_call, None, None)


def on_name_lost(_connection, _name):
    print(f"window-view-dbus: {BUS_NAME} is taken (is KWin running?)", file=sys.stderr)
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
