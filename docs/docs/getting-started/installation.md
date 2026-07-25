---
sidebar_position: 1
---

# Installation

ShojiWM installs from source with a single script, `dist/install.sh`. It builds
everything, installs the compositor and its TypeScript runtime, drops in a
default user config, and registers a Wayland session so ShojiWM shows up in your
login manager.

:::info[Packaged installs are coming]
Distribution packages (AUR and similar) are planned for **just before the
official release**. Until then, install from source as described below.
:::

## Prerequisites

- A Linux system with a working Wayland / DRM setup
- A recent Rust toolchain (`cargo`)
- The following native libraries (with their development headers), which ShojiWM
  links against:
  - `libwayland`
  - `libxkbcommon`
  - `libudev`
  - `libinput`
  - `libgbm`
  - `libseat`
  - `xwayland` — the Xwayland server itself (driven by `xwayland-satellite` below)
- [`xwayland-satellite`](https://github.com/Supreeeme/xwayland-satellite) — for
  running X11 / Xwayland applications (see the note below)
- `sudo` — the installer copies files into `/usr` and registers the session

:::note[Installing the native libraries]
Package names vary by distribution. For example:

```bash
# Debian / Ubuntu
sudo apt install libwayland-dev libxkbcommon-dev libudev-dev libinput-dev \
  libgbm-dev libseat-dev xwayland

# Arch Linux
sudo pacman -S wayland libxkbcommon systemd-libs libinput mesa seatd xorg-xwayland
```

:::

:::note[xwayland-satellite is required]
ShojiWM uses `xwayland-satellite` to run X11 applications. The recommended way to
install it is to clone its repository and install directly with Cargo:

```bash
git clone https://github.com/Supreeeme/xwayland-satellite.git
cd xwayland-satellite
cargo install --path ./
```

This places the `xwayland-satellite` binary on your `PATH` (typically under
`~/.cargo/bin`). Install it before starting a session.

**Recommended for ShojiWM:** a ShojiWM-specific fork with hotfixes is available on
the `shojiwm` branch of
[`bea4dev/xwayland-satellite`](https://github.com/bea4dev/xwayland-satellite/tree/shojiwm).
It includes an experimental fix for an issue where Unity tabs cannot be grabbed
and moved. If you want these fixes and other hotfix support, install that branch
instead:

```bash
git clone -b shojiwm https://github.com/bea4dev/xwayland-satellite.git
cd xwayland-satellite
cargo install --path ./
```

:::

## Install

```bash
git clone https://github.com/bea4dev/ShojiWM.git
cd ShojiWM
./dist/install.sh
```

The script will prompt for `sudo` when it needs to copy files into system
directories. It performs the following:

- **Builds** the compositor and the xdg-desktop-portal backend with `cargo`.
- Embeds the Deno/V8 TypeScript engine into the compositor through RustyScript;
  Node.js is not required at runtime.
- Installs the compositor to `/usr/bin/shoji_wm` and the runtime to
  `/usr/lib/shojiwm`.
- Creates a **default user config** at `~/.config/shojiwm` (an existing config is
  left untouched).
- Registers a **Wayland session entry**, so **ShojiWM appears in your login
  manager** — just pick it on the login screen.
- Installs the ShojiWM **xdg-desktop-portal** backend (screen casting, etc.).

### Install options

| Flag          | Effect                                           |
| ------------- | ------------------------------------------------ |
| `--no-build`  | Skip the `cargo` build and use existing binaries |
| `--no-portal` | Don't install the xdg-desktop-portal backend     |
| `--no-config` | Don't create or update the user config           |

Run `./dist/install.sh --help` to see this list.

## NixOS / flakes

ShojiWM also provides an experimental Nix flake. The flake is intended to keep
the same split as the source installer:

- the compositor, portal backend, and TypeScript runtime live in the Nix store
- your editable TypeScript config lives in `~/.config/shojiwm`
- development still uses `--dev` and the source tree directly

:::warning[Experimental]
The NixOS support is new. Expect rough edges and check the latest repository
state before relying on it for a daily-driver system.
:::

### NixOS module (install)

Add ShojiWM as a flake input:

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    shojiwm.url = "github:bea4dev/ShojiWM";
  };

  outputs = { nixpkgs, shojiwm, ... }: {
    nixosConfigurations.your-host = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        shojiwm.nixosModules.default
        {
          programs.shojiwm = {
            enable = true;
            initConfig = {
              enable = true;
              users = [ "your-user" ];
            };
          };
        }
      ];
    };
  };
}
```

Then apply your system configuration:

```bash
sudo nixos-rebuild switch --flake .#your-host
```

The module installs:

- the `shoji_wm` compositor
- `xdg-desktop-portal-shojiwm`
- the Wayland session entry for display managers
- the ShojiWM portal preference for screen capture
- `xwayland-satellite`, when available from your nixpkgs

With `programs.shojiwm.initConfig.enable = true`, the module also initializes
the ShojiWM TypeScript config directory for the listed users during system
activation. It copies the default config only when `src/index.tsx` does not
exist yet. Existing user config files such as `src/index.tsx` and
`src/window-manager.ts` are kept. The generated support files are refreshed on
every rebuild:

- `node_modules/shoji_wm`, linked to the current Nix store package
- `package.json`
- `tsconfig.json`

These files keep editor diagnostics and standalone TypeScript type checking in
sync with the installed ShojiWM version. The compositor itself resolves and
transpiles the config through its embedded RustyScript runtime.

If you do not want the NixOS module to manage the config directory, omit
`initConfig` and initialize the editable TypeScript config manually:

```bash
nix run github:bea4dev/ShojiWM#init-config
```

This creates `~/.config/shojiwm` if it does not already exist, and links
`~/.config/shojiwm/node_modules/shoji_wm` to the package in the Nix store. Your
config remains writable and can still be hot-reloaded.

### Development shell

From the ShojiWM source tree:

```bash
nix develop
cargo run --release -p shoji_wm -- --dev
```

Run `npm ci` separately only when you need the repository's TypeScript type
checking or documentation development tools.

`--dev` keeps using the repository checkout:

```text
./tools/decoration-runtime.ts
./packages/config/src/index.tsx
./packages/shoji_wm
```

This means you can keep the current fast edit-and-run workflow while using Nix
only to provide the native build dependencies.

### xwayland-satellite fork

If you need the ShojiWM-specific `xwayland-satellite` fork, override the package
from the NixOS module:

```nix
{
  programs.shojiwm = {
    enable = true;
    xwaylandSatellite.package =
      inputs.xwayland-satellite-shojiwm.packages.${pkgs.system}.default;
  };
}
```

Where your flake inputs include the fork, for example:

```nix
{
  inputs.xwayland-satellite-shojiwm.url =
    "github:bea4dev/xwayland-satellite/shojiwm";
}
```

You can also set `programs.shojiwm.xwaylandSatellite.package` to any package that
provides a `bin/xwayland-satellite` executable.

## Running

- **From your login manager:** choose **ShojiWM** as the session and log in.
- **From a TTY:** run `shoji_wm --tty`.
- **Development (nested window):** run `cargo run --release -p shoji_wm -- --dev`
  from the source tree — handy for iterating without leaving your current session.

## Optional: desktop shell

ShojiWM is just the compositor — it does not ship a bar, launcher, or other shell
UI on its own. A standard shell implementation is provided separately:

- **shoji-bar-2** — [github.com/bea4dev/shoji-bar-2](https://github.com/bea4dev/shoji-bar-2)

Follow the setup instructions in that repository's `README.md` to install and
enable it. (The default ShojiWM config already launches `shoji-bar-2` if it is
present.)
