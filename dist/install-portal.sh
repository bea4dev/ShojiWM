#!/usr/bin/env bash
# Install xdg-desktop-portal-shojiwm system-wide.
#
# Builds the release binary (skipped if --no-build is passed), then installs
# the binary and three companion files via `sudo`. Re-running is safe; install(1)
# overwrites existing copies in place.
#
# Usage:
#   dist/install-portal.sh             # build + install
#   dist/install-portal.sh --no-build  # install only (use existing target/release build)

: "${XDG_CONFIG_HOME:=$HOME/.config}"

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

BUILD=1
for arg in "$@"; do
    case "$arg" in
        --no-build) BUILD=0 ;;
        -h|--help)
            sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

if [[ $BUILD -eq 1 ]]; then
    echo ">> cargo build --release -p xdg-desktop-portal-shojiwm"
    cargo build --release -p xdg-desktop-portal-shojiwm
fi

BIN="$REPO_ROOT/target/release/xdg-desktop-portal-shojiwm"
if [[ ! -x "$BIN" ]]; then
    echo "binary not found: $BIN" >&2
    echo "run without --no-build, or run cargo build --release -p xdg-desktop-portal-shojiwm first" >&2
    exit 1
fi

echo ">> installing files (sudo)"
sudo install -Dm755 "$BIN" \
    /usr/bin/xdg-desktop-portal-shojiwm
sudo install -Dm644 "$REPO_ROOT/dist/shojiwm.portal" \
    /usr/share/xdg-desktop-portal/portals/shojiwm.portal
sudo install -Dm644 "$REPO_ROOT/dist/org.freedesktop.impl.portal.desktop.shojiwm.service" \
    /usr/share/dbus-1/services/org.freedesktop.impl.portal.desktop.shojiwm.service
sudo install -Dm644 "$REPO_ROOT/dist/xdg-desktop-portal-shojiwm.service" \
    /usr/lib/systemd/user/xdg-desktop-portal-shojiwm.service

echo ">> writing user portals.conf"
mkdir -p "$XDG_CONFIG_HOME/xdg-desktop-portal"
cat > "$XDG_CONFIG_HOME/xdg-desktop-portal/shojiwm-portals.conf" <<'EOF'
[preferred]
default=gtk
org.freedesktop.impl.portal.ScreenCast=shojiwm
org.freedesktop.impl.portal.Screenshot=shojiwm
EOF

echo ">> reloading systemd + restarting xdg-desktop-portal"
systemctl --user daemon-reload
systemctl --user stop xdg-desktop-portal-shojiwm.service 2>/dev/null || true
systemctl --user restart xdg-desktop-portal

echo ""
echo "done. tail logs with:"
echo "  journalctl --user -fu xdg-desktop-portal-shojiwm -u xdg-desktop-portal"
echo ""
echo "note: this config routes ScreenCast and Screenshot to shojiwm. Other portal"
echo "interfaces use xdg-desktop-portal-gtk via default=gtk."
