---
sidebar_position: 1.5
---

# Default config

ShojiWM ships with a complete default config (`packages/config/src/index.tsx`,
installed to `~/.config/shojiwm/src/index.tsx`). This page describes what it does
**out of the box** — keybindings, window behavior, multi-monitor layout, input,
effects, and the programs it launches. It is also the best worked example: to
change anything, edit `~/.config/shojiwm/src/index.tsx` and hot reload with
`Super` + `Shift` + `R`.

:::note[Some keybindings launch specific programs]
The default config is the maintainer's personal setup, so several shortcuts open
specific apps (kitty, Google Chrome, Discord, Dolphin) and rely on helper tools
(hyprshot, swappy, cliphist, fcitx5, shoji-bar-2). If you don't have one, the
shortcut simply does nothing — rebind it to your preferred program. See
[Keybindings & Pointer](./keybindings-and-pointer.md).
:::

## Keybindings

`Super` is the modifier (the "Windows" key). Note that **Caps Lock acts as Ctrl**
by default (see [Input](#input)).

### Launching programs

| Shortcut | Action |
| --- | --- |
| `Super` + `T` | Terminal (kitty) |
| `Super` + `B` | Browser (Google Chrome, Wayland) |
| `Super` + `D` | Discord |
| `Super` + `E` | File manager (Dolphin) |
| `Super` + `A` | Toggle the start menu (shoji-bar-2) |
| `Super` (tap) | Toggle the start menu — a quick tap with no other key |
| `Super` + `V` | Toggle clipboard history (shoji-bar-2) |
| `Super` + `P` | Screenshot a region (hyprshot → swappy) |
| `Super` + `Ctrl` + `P` | Screenshot a region, freezing the screen first |

### Window management

| Shortcut | Action |
| --- | --- |
| `Super` + drag | Move a window — hold `Super` and drag anywhere on it |
| `Super` + `Q` | Close the focused window |
| `Super` + `M` | Toggle maximize on the focused window |
| `Super` + `S` | Toggle tiling mode for the current workspace |

### Tiling & workspaces

| Shortcut | Action |
| --- | --- |
| `Super` + `←` / `→` | Move focus to the left / right tile |
| `Super` + `Ctrl` + `←` / `→` | Move focus to the left / right tile (alias) |
| `Super` + `Shift` + `←` / `→` | Move the focused tile left / right |
| `Super` + `Ctrl` + `↑` / `↓` | Switch to the previous / next workspace |
| `Super` + `Shift` + `↑` / `↓` | Move the focused window to the previous / next workspace |

### System (built into the compositor)

These are hard-wired in the compositor itself, not defined by the config — so
they keep working even if your config is broken.

| Shortcut | Action |
| --- | --- |
| `Super` + `Shift` + `R` | **Hot reload the config** — apply edits to `index.tsx` without restarting (see [Overview → Hot reload](./overview.md#hot-reload)) |
| `Super` + `Shift` + `Q` | Quit the ShojiWM session |

### Debug

| Shortcut | Action |
| --- | --- |
| `Super` + `Shift` + `F` | Toggle the FPS / frame-time overlay |

## Window appearance

Each window gets a title bar with the app icon, the window title, and
minimize / maximize / close buttons. The border is **gold when focused**
(`#d7ba7d`) and gray otherwise (`#4f5666`), with rounded corners.

- **Terminals** (kitty, ghostty) get a translucent *liquid-glass* blurred
  background instead of a solid title bar.
- **Fullscreen** windows drop all chrome (border, title bar, rounded corners) and
  render edge-to-edge. This bare path lets the compositor hand the buffer
  straight to the display (direct scanout) and permits tearing, for the lowest
  latency in games.

## Tiling & workspaces

Window management is handled by the **HybridWindowManager**, which mixes tiling
and floating per workspace:

- Workspaces start in **floating** mode (windows placed freely, draggable with
  `Super` + drag). Press `Super` + `S` to toggle **tiling** for the current
  workspace, where windows are arranged automatically side by side.
- Multiple workspaces per monitor, navigated with `Super` + `Ctrl` + `↑`/`↓`, and
  windows moved between them with `Super` + `Shift` + `↑`/`↓`.
- Open/close, move, resize, and workspace switches are animated.

The default config also exposes the workspace layout over an IPC socket so an
external bar (shoji-bar-2) can render workspace indicators and react to changes.

## Multi-monitor

The default `output.configure` sets per-connector scales and extends all displays
left to right with automatic positioning. When an external monitor is connected
(a docked setup), the laptop's built-in panels are turned off.

:::tip
The connector names (`eDP-1`, `HDMI-A-1`, `DP-1`, …) and scale values in the
default config are the maintainer's hardware. You will almost certainly want to
adjust these for your own monitors — see [Outputs](./outputs.md).
:::

## Input

Defaults applied by `input.configure`:

- **Keyboard** — Caps Lock acts as Ctrl (`caps:ctrl_modifier`); key repeat rate
  `60`/s with a `250 ms` delay.
- **Touchpad** — tap-to-click, natural scrolling, two-finger scrolling
  (factor `0.3`), and disable-while-typing.
- **Pointer** — flat acceleration profile (no pointer acceleration).

See [Input devices](./input.md) to change these.

## Visual effects

- A **background blur** is provided for clients that request it via the
  `ext-background-effect-v1` protocol (e.g. translucent apps that ask the
  compositor to blur behind them).
- **Layer surfaces** (bars, docks) and **layer popups** (menus) are blurred
  behind, unless a surface opts out with the `no_blur` namespace.

See [Effects](./effects.md) for how these are built.

## Startup programs & environment

On launch the default config:

- Sets environment variables for Wayland and the fcitx5 input method
  (`QT_QPA_PLATFORM`, `QT_IM_MODULE`, etc.).
- Starts **fcitx5** (input method), **shoji-bar-2** (the shell), and **cliphist**
  clipboard-history watchers.

These expect the corresponding programs to be installed. Remove or adjust the
`process` / `env` calls in your config if you use different tools — see
[Processes & Environment](./processes-and-env.md).

## Customizing

Your config lives at `~/.config/shojiwm/src/index.tsx`. Edit it and hot reload
with `Super` + `Shift` + `R`; the window-manager state (workspaces, tiling) is
preserved across reloads. From here,
the rest of this section documents every API the default config uses.
