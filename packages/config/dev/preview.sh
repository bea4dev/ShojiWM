#!/usr/bin/env bash
# Run the window decorations in a nested ShojiWM (a window on the current
# desktop, winit backend), isolated from the running session:
#
#   - nothing is published to the systemd / D-Bus activation environment,
#   - the session log goes to $PREVIEW_HOME/shoji_wm/logs, not ~/shoji_wm/logs,
#   - the config is ./preview.tsx: no shell, services or key bindings.
#
#   dev/preview.sh                 # start the nested compositor
#   dev/preview.sh theme light     # send it a scheme (light, dark, or a JSON file)
#   dev/preview.sh run kwrite      # start an app in it
#   dev/preview.sh reload          # reload it after editing the config
#
# SHOJI_WM_BIN picks the compositor binary (default: shoji_wm on PATH), and
# PREVIEW_DISPLAY the nested compositor's socket when there are several.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bin="${SHOJI_WM_BIN:-shoji_wm}"
preview_home="${PREVIEW_HOME:-${XDG_RUNTIME_DIR:-/tmp}/shojiwm-preview}"

# The nested compositor picks the first free socket name; the newest ShojiWM
# socket other than this session's is taken to be it (PREVIEW_DISPLAY wins).
nested_display() {
  if [[ -n "${PREVIEW_DISPLAY:-}" ]]; then
    echo "$PREVIEW_DISPLAY"
    return
  fi
  local sock
  for sock in $(ls -t "$XDG_RUNTIME_DIR"/shojiwm.wayland-*.sock 2>/dev/null); do
    local name="${sock##*/shojiwm.}"
    name="${name%.sock}"
    if [[ "$name" != "${WAYLAND_DISPLAY:-}" && -S "$XDG_RUNTIME_DIR/$name" ]]; then
      echo "$name"
      return
    fi
  done
  echo "no nested ShojiWM found (start one with: $0)" >&2
  exit 1
}

case "${1:-start}" in
  start)
    mkdir -p "$preview_home"
    export XDG_CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
    export XDG_DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
    export SHOJI_PUBLISH_ACTIVATION_ENV=0
    unset SHOJIWM_SOCKET
    # The log directory follows HOME; the config and data stay the real ones.
    HOME="$preview_home" exec "$bin" --config "$here/preview.tsx"
    ;;
  theme)
    # As shoji-bar-4 sends it: `theme.set` with the scheme's roles.
    exec python3 "$here/send-theme.py" "${2:-dark}" \
      "${XDG_RUNTIME_DIR:-/tmp}/shojiwm-md3e-preview.sock"
    ;;
  run)
    shift
    WAYLAND_DISPLAY="$(nested_display)" DISPLAY= exec "$@"
    ;;
  reload)
    SHOJIWM_SOCKET="$XDG_RUNTIME_DIR/shojiwm.$(nested_display).sock" exec "$bin" --reload
    ;;
  *)
    echo "usage: $0 [start | theme light|dark|<file.json> | run <command...> | reload]" >&2
    exit 2
    ;;
esac
