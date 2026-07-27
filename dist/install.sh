#!/usr/bin/env bash
# Install ShojiWM from the current source tree.
#
# This is intentionally a plain source install script, not a distro package.
# It installs the compositor, the TypeScript runtime files, a default user
# config if one does not already exist, a Wayland session entry, and the
# ShojiWM xdg-desktop-portal backend unless --no-portal is passed.
#
# Usage:
#   dist/install.sh
#   dist/install.sh --dev        build quickly to allow faster testing for features (release-fast: no LTO, incremental)
#   dist/install.sh --debug      hardened allocator (heap-debug feature) and full debuginfo, for chasing heap corruption; combines with --dev
#   dist/install.sh --no-build
#   dist/install.sh --no-portal
#   dist/install.sh --no-config

set -euo pipefail

: "${XDG_CONFIG_HOME:=$HOME/.config}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

BUILD=1
DEBUG=0
DEV=0
INSTALL_PORTAL=1
INSTALL_CONFIG=1

for arg in "$@"; do
    case "$arg" in
        --debug) DEBUG=1 ;;
        --dev) DEV=1 ;;
        --no-build) BUILD=0 ;;
        --no-portal) INSTALL_PORTAL=0 ;;
        --no-config) INSTALL_CONFIG=0 ;;
        -h|--help)
            awk 'NR == 1{next} /^#/{sub(/^# ?/, ""); print; next} {exit}' "$0"
            exit 0
            ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

# --dev selects the release-fast profile; binaries land in a different
# target subdirectory, so resolve it here for the build and install steps.
PROFILE_DIR=release
PROFILE_FLAG=--release
if [[ $DEV -eq 1 ]]; then
    PROFILE_DIR=release-fast
    PROFILE_FLAG="--profile release-fast"
fi

if [[ $BUILD -eq 1 ]]; then
    CARGO_ARGS=($PROFILE_FLAG -p shoji_wm -p xdg-desktop-portal-shojiwm)
    if [[ $DEBUG -eq 1 ]]; then
        # Hardened mimalloc (guard pages, encoded free lists): heap
        # corruption aborts at the faulting write instead of detonating
        # later. Expect higher memory use and a small slowdown.
        CARGO_ARGS+=(--features shoji_wm/heap-debug)
        # Full debuginfo so core dumps symbolize cleanly. Cargo maps
        # profile names to env keys with hyphens as underscores.
        export CARGO_PROFILE_RELEASE_DEBUG=true
        export CARGO_PROFILE_RELEASE_FAST_DEBUG=true
    fi
    echo ">> cargo build ${CARGO_ARGS[*]}"
    cargo build "${CARGO_ARGS[@]}"
fi

SHOJI_BIN="$REPO_ROOT/target/$PROFILE_DIR/shoji_wm"
PORTAL_BIN="$REPO_ROOT/target/$PROFILE_DIR/xdg-desktop-portal-shojiwm"

if [[ ! -x "$SHOJI_BIN" ]]; then
    echo "binary not found: $SHOJI_BIN" >&2
    echo "run without --no-build, or run cargo build $PROFILE_FLAG -p shoji_wm first" >&2
    exit 1
fi

if [[ $INSTALL_PORTAL -eq 1 && ! -x "$PORTAL_BIN" ]]; then
    echo "binary not found: $PORTAL_BIN" >&2
    echo "run without --no-build, or run cargo build $PROFILE_FLAG -p xdg-desktop-portal-shojiwm first" >&2
    exit 1
fi

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

RUNTIME_STAGE="$STAGE/shojiwm-runtime"
mkdir -p "$RUNTIME_STAGE/packages" "$RUNTIME_STAGE/tools"

cp -a "$REPO_ROOT/packages/shoji_wm" "$RUNTIME_STAGE/packages/"
cp "$REPO_ROOT/tools/decoration-runtime.ts" "$RUNTIME_STAGE/tools/"

echo ">> installing compositor files (sudo)"
sudo rm -rf /usr/lib/shojiwm
sudo install -Dm755 "$SHOJI_BIN" /usr/bin/shoji_wm
sudo mkdir -p /usr/lib/shojiwm
sudo cp -a "$RUNTIME_STAGE/." /usr/lib/shojiwm/
sudo install -Dm644 "$REPO_ROOT/dist/shojiwm.desktop" \
    /usr/share/wayland-sessions/shojiwm.desktop

echo ">> installing default config template (sudo)"
sudo rm -rf /usr/share/shojiwm/default-config
sudo mkdir -p /usr/share/shojiwm/default-config
sudo cp -a "$REPO_ROOT/packages/config/." /usr/share/shojiwm/default-config/

if [[ $INSTALL_CONFIG -eq 1 ]]; then
    USER_CONFIG_DIR="$XDG_CONFIG_HOME/shojiwm"
    CREATED_CONFIG=0
    if [[ ! -e "$USER_CONFIG_DIR/src/index.tsx" ]]; then
        echo ">> creating user config at $USER_CONFIG_DIR"
        mkdir -p "$USER_CONFIG_DIR"
        cp -a "$REPO_ROOT/packages/config/." "$USER_CONFIG_DIR/"
        CREATED_CONFIG=1
    else
        echo ">> keeping existing user config at $USER_CONFIG_DIR"
    fi

    mkdir -p "$USER_CONFIG_DIR/node_modules"
    ln -sfn /usr/lib/shojiwm/packages/shoji_wm "$USER_CONFIG_DIR/node_modules/shoji_wm"
    if [[ $CREATED_CONFIG -eq 1 || ! -e "$USER_CONFIG_DIR/package.json" ]]; then
        cat > "$USER_CONFIG_DIR/package.json" <<'EOF'
{
  "name": "shojiwm-user-config",
  "private": true,
  "type": "module",
  "dependencies": {
    "shoji_wm": "file:/usr/lib/shojiwm/packages/shoji_wm"
  }
}
EOF
    fi
    if [[ $CREATED_CONFIG -eq 1 || ! -e "$USER_CONFIG_DIR/tsconfig.json" ]]; then
        cat > "$USER_CONFIG_DIR/tsconfig.json" <<'EOF'
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "Bundler",
    "jsx": "react-jsx",
    "jsxImportSource": "shoji_wm",
    "strict": true,
    "verbatimModuleSyntax": true,
    "noEmit": true
  }
}
EOF
    fi
fi

if [[ $INSTALL_PORTAL -eq 1 ]]; then
    echo ">> installing xdg-desktop-portal-shojiwm files (sudo)"
    sudo install -Dm755 "$PORTAL_BIN" /usr/bin/xdg-desktop-portal-shojiwm
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
EOF

    echo ">> reloading systemd user services"
    #sleep is needed to prevent dbus errors
    sleep 1
    systemctl --user daemon-reload
    systemctl --user stop xdg-desktop-portal-shojiwm.service 2>/dev/null || true
    systemctl --user restart xdg-desktop-portal 2>/dev/null || true
fi

echo ""
echo "done."
echo "Development run: cargo run --profile release-fast -p shoji_wm -- --dev"
echo "Installed run: select ShojiWM in your display manager, or run: shoji_wm --tty"
