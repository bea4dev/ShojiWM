"""Sends a color scheme to the preview, the way shoji-bar-4 does: `theme.set`."""
import json
import socket
import sys

# Material 3's baseline light scheme (seed #6750a4); "dark" lets the preview
# go back to its baseline dark one.
LIGHT = {
    "dark": False,
    "primary": "#6750a4", "onPrimary": "#ffffff",
    "primaryContainer": "#eaddff", "onPrimaryContainer": "#21005d",
    "secondary": "#625b71", "onSecondary": "#ffffff",
    "secondaryContainer": "#e8def8", "onSecondaryContainer": "#1d192b",
    "tertiary": "#7d5260", "tertiaryContainer": "#ffd8e4", "onTertiaryContainer": "#31111d",
    "error": "#b3261e", "onError": "#ffffff",
    "errorContainer": "#f9dedc", "onErrorContainer": "#410e0b",
    "surface": "#fef7ff", "surfaceDim": "#ded8e1", "surfaceBright": "#fef7ff",
    "surfaceContainerLowest": "#ffffff", "surfaceContainerLow": "#f7f2fa",
    "surfaceContainer": "#f3edf7", "surfaceContainerHigh": "#ece6f0",
    "surfaceContainerHighest": "#e6e0e9",
    "onSurface": "#1d1b20", "onSurfaceVariant": "#49454f",
    "inverseSurface": "#322f35", "inverseOnSurface": "#f5eff7",
    "outline": "#79747e", "outlineVariant": "#cac4d0", "scrim": "#000000",
}

which, path = sys.argv[1], sys.argv[2]
if which == "light":
    scheme = LIGHT
elif which == "dark":
    scheme = {"dark": True}
else:
    with open(which) as f:
        scheme = json.load(f)
with socket.socket(socket.AF_UNIX) as s:
    s.connect(path)
    s.sendall((json.dumps({"method": "theme.set", "params": scheme}) + "\n").encode())
