#!/usr/bin/env python3
"""shoji-keys: print every ShojiWM key binding, grouped.

The list comes from the same table that binds the keys (../keymap.ts), so it
is always up to date. Super+/ shows the same list on screen.
"""

import json
import os
import socket
import sys

HELP = """\
Usage: shoji-keys [GROUP]
       shoji-keys --help

Print every ShojiWM key binding, grouped (Apps, Windows, Tiling, Floating,
Workspaces, Switchers, Screenshots, Session, Debug).

  GROUP   Print only that group, for example: shoji-keys tiling

Some keys do two things: one when the workspace tiles, one when it floats.
Super+S switches the mode. Those keys are in both the Tiling and the Floating
groups.

Super+/ shows the same list on screen.

The list comes from the running config: it asks ShojiWM over its IPC socket,
or reads $XDG_RUNTIME_DIR/shojiwm-keys.json. Edit the keys in
~/.config/shojiwm/src/keymap.ts, then run: shoji_wm --reload"""


def runtime_dir() -> str:
    return os.environ.get("XDG_RUNTIME_DIR", "/tmp")


def from_ipc() -> dict | None:
    display = os.environ.get("WAYLAND_DISPLAY", "wayland-0")
    path = f"{runtime_dir()}/shojiwm-{display}.sock"
    try:
        with socket.socket(socket.AF_UNIX) as sock:
            sock.settimeout(2)
            sock.connect(path)
            sock.sendall(b'{"id":1,"method":"keys.list"}\n')
            buffer = b""
            while True:
                chunk = sock.recv(65536)
                if not chunk:
                    return None
                buffer += chunk
                *lines, buffer = buffer.split(b"\n")
                for line in lines:
                    message = json.loads(line)
                    if message.get("id") == 1:
                        return message.get("result")
    except (OSError, ValueError):
        return None


def from_file() -> dict | None:
    try:
        with open(f"{runtime_dir()}/shojiwm-keys.json") as file:
            return json.load(file)
    except (OSError, ValueError):
        return None


def main() -> int:
    args = sys.argv[1:]
    if args and args[0] in ("-h", "--help", "help"):
        print(HELP)
        return 0
    data = from_ipc() or from_file()
    if not data:
        print("shoji-keys: ShojiWM is not running, and no saved list was found.", file=sys.stderr)
        return 1
    rows = data.get("rows", [])
    groups = data.get("groups") or list(dict.fromkeys(row["group"] for row in rows))
    only = args[0].lower() if args else None
    width = max((len(row["keys"]) for row in rows), default=20) + 3
    printed = False
    for group in groups:
        if only and group.lower() != only:
            continue
        lines = [row for row in rows if row["group"] == group]
        if not lines:
            continue
        if printed:
            print()
        print(group.upper())
        for row in lines:
            print(f"  {row['keys']:<{width}}{row['description']}")
        printed = True
    if not printed:
        print(f"shoji-keys: no group called {args[0]!r}. Try: shoji-keys --help", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
