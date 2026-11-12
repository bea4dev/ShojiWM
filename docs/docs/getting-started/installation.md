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
  - `libxcb` and `xcb-util-cursor` — used by the built-in xwayland-satellite
  - `xwayland` — the Xwayland server itself, for X11 applications (see the note
    below)
- `sudo` — the installer copies files into `/usr` and registers the session

:::warning[Minimum versions]
ShojiWM needs newer system packages than some LTS distributions ship:

- `libwayland` **1.23** or newer — required at runtime; with an older one
  ShojiWM exits at startup with `NoWaylandLib`
- `xwayland` **23.1** or newer — for the built-in xwayland-satellite (older
  versions can use `SHOJI_XWAYLAND_SATELLITE=off` instead)
- PipeWire **0.3.77** or newer — only to build the xdg-desktop-portal backend
  (skip it with `--no-portal`)

On Ubuntu this means **25.04 or newer**; Ubuntu 22.04 and 24.04 are not
supported. Debian 13 (trixie) or newer also meets these requirements.
:::

:::note[Installing the native libraries]
Package names vary by distribution. For example:

```bash
# Debian / Ubuntu
sudo apt install libwayland-dev libxkbcommon-dev libudev-dev libinput-dev \
  libgbm-dev libseat-dev libxcb1-dev libxcb-cursor-dev xwayland

# Arch Linux
sudo pacman -S wayland libxkbcommon systemd-libs libinput mesa seatd libxcb \
  xcb-util-cursor xorg-xwayland
```

:::

:::note[X11 applications]
ShojiWM runs X11 applications through
[`xwayland-satellite`](https://github.com/Supreeeme/xwayland-satellite), which is
built into ShojiWM: it uses the ShojiWM-specific fork on the `shojiwm` branch of
[`bea4dev/xwayland-satellite`](https://github.com/bea4dev/xwayland-satellite/tree/shojiwm)
and runs inside the compositor, so there is nothing to install besides
`xwayland`. If Xwayland stops, ShojiWM restarts it on the same `DISPLAY`.

For debugging, a separately installed `xwayland-satellite` can be run as its own
process instead:

| Environment variable | Effect |
| --- | --- |
| `SHOJI_XWAYLAND_SATELLITE=external` | run the `xwayland-satellite` found in `PATH` |
| `SHOJI_XWAYLAND_SATELLITE_PATH=/path/to/xwayland-satellite` | run that binary (also `--xwayland-satellite-path`) |
| `SHOJI_XWAYLAND_SATELLITE=off` | use Smithay's built-in Xwayland support instead of xwayland-satellite |
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

### Screen-sharing picker

By default the portal asks which screen or window to share in its own window.
To use another program instead, set `SHOJIWM_PORTAL_PICKER` to a command in
the portal's environment (for example in `~/.config/environment.d/`, since the
portal runs as a systemd user service). The portal runs it with `sh -c`,
writes one JSON object to its stdin (`app_id`, `prefer` = `"window"` or
`"screen"`, and `sources`: outputs with `id`, `kind: "output"`, `name`,
`description`, `x`, `y`, `width`, `height`, `refresh_mhz`, and windows with
`id`, `kind: "window"`, `title`, `app_id`), then closes stdin. The program
prints the `id` of the chosen source on the first line of stdout and exits
with status 0. Any other result cancels the request. If the requesting app
closes the session first, the portal kills the program.

## NixOS / flakes

ShojiWM also provides an experimental Nix flake. The flake is intended to keep
the same split as the source installer:

- the compositor, portal backend, embedded Deno/V8 engine, and TypeScript
  runtime sources live in the Nix store
- your editable TypeScript config lives in `~/.config/shojiwm`
- development still uses `--dev` and the source tree directly

Node.js is not part of the installed compositor runtime. Nix downloads the
versioned `rusty_v8` archive as a fixed-output dependency while building
ShojiWM, then links it into the compositor binary.

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

With `programs.shojiwm.initConfig.enable = true`, the module also initializes
the ShojiWM TypeScript config directory for the listed users during system
activation. It copies the default config only when `src/index.tsx` does not
exist yet. Existing user config files such as `src/index.tsx` and
`src/window-manager.ts` are kept. The generated support files are refreshed on
every rebuild:

- `node_modules/shoji_wm`, linked to the current Nix store TypeScript package
- `package.json`
- `tsconfig.json`

These files keep editor diagnostics and standalone TypeScript type checking in
sync with the installed ShojiWM version. They are not Node runtime
dependencies: the compositor resolves and transpiles the config through its
embedded RustyScript/Deno runtime.

If you do not want the NixOS module to manage the config directory, omit
`initConfig` and initialize the editable TypeScript config manually:

```bash
nix run github:bea4dev/ShojiWM#init-config
```

This creates `~/.config/shojiwm` if it does not already exist, and links
`~/.config/shojiwm/node_modules/shoji_wm` to the package in the Nix store. Your
config remains writable and can still be hot-reloaded with `Super` + `Shift` +
`R` (see [Overview → Hot reload](../configuration/overview.md#hot-reload)).

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
to provide the native build dependencies and the pinned `rusty_v8` archive.

### External xwayland-satellite

xwayland-satellite is built into ShojiWM. To run a separately packaged
`xwayland-satellite` as its own process instead (for debugging, or to try a
different branch), enable it in the NixOS module:

```nix
{
  programs.shojiwm = {
    enable = true;
    xwaylandSatellite.enable = true;
    xwaylandSatellite.package =
      inputs.xwayland-satellite-shojiwm.packages.${pkgs.system}.default;
  };
}
```

Where your flake inputs include the package, for example:

```nix
{
  inputs.xwayland-satellite-shojiwm.url =
    "github:bea4dev/xwayland-satellite/shojiwm";
}
```

`programs.shojiwm.xwaylandSatellite.package` accepts any package that provides a
`bin/xwayland-satellite` executable; it defaults to `pkgs.xwayland-satellite`.

## Running

- **From your login manager:** choose **ShojiWM** as the session and log in.
- **From a TTY:** run `shoji_wm --tty`.
- **Development (nested window):** run `cargo run --release -p shoji_wm -- --dev`
  from the source tree — handy for iterating without leaving your current session.

## Optional: desktop shell

ShojiWM is just the compositor — it does not ship a bar, launcher, or other shell
UI on its own. A standard shell implementation is provided separately:

- **shoji-bar-4** — [github.com/bea4dev/shoji-bar-4](https://github.com/bea4dev/shoji-bar-4), a Material 3 Expressive shell that goes with the default config's window frames

Follow the setup instructions in that repository's `README.md` to install and
enable it. (The default ShojiWM config already launches it from
`~/.config/shoji-bar-4/target/release/shoji-bar-4` if it is present.)
