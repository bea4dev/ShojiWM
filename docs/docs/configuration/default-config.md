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
(hyprshot, swappy, cliphist, fcitx5, [shoji-bar-4](https://github.com/bea4dev/shoji-bar-4)).
If you don't have one, the
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
| `Super` + `A` | Toggle the launcher (shoji-bar-4) |
| `Super` (tap) | Toggle the launcher — a quick tap (under 300 ms) with no other key |
| `Super` + `N` | Toggle quick settings (shoji-bar-4) |
| `Super` + `C` | Toggle the calendar (shoji-bar-4) |
| `Super` + `V` | Toggle clipboard history (shoji-bar-4) |
| `Super` + `L` | Lock the session (shoji-bar-4) |
| `Super` + `P` | Screenshot a region (hyprshot → swappy) |
| `Super` + `Ctrl` + `P` | Screenshot a region, freezing the screen first |

### Window management

| Shortcut | Action |
| --- | --- |
| `Super` + drag | Move a window — hold `Super` and drag anywhere on it |
| `Super` (hold) | Free the cursor from a game that locks it — [details](./cursor.md#ignoring-app-pointer-locks) |
| `Super` + `Q` | Close the focused window |
| `Super` + `M` | Toggle maximize on the focused window |
| `Super` + `F` | Toggle fullscreen on the focused window (compositor-side, see below) |
| `Super` + `S` | Toggle tiling mode for the current workspace |

:::tip[`Super` + `F` and Wine / Proton games]
Most applications toggle fullscreen themselves, so you rarely need this key.
It exists for games running under Wine or Proton (through xwayland-satellite):
Wine treats any window whose rectangle covers the whole monitor as
fullscreen, so a game whose "windowed" mode uses the desktop resolution can
never leave fullscreen on its own, and its in-game toggle appears to do
nothing. `Super` + `F` forces the state from the compositor side; Wine
accepts a state change the window manager initiates and resizes the window
to match. If the game is set to fullscreen it will re-enter fullscreen a
moment later — switch the game to windowed mode first, then press the key.
:::

### Tiling & workspaces

| Shortcut | Action |
| --- | --- |
| `Super` + `←` / `→` | Move focus to the left / right tile |
| `Super` + `Ctrl` + `←` / `→` | Move focus to the left / right tile (alias) |
| `Super` + `Shift` + `←` / `→` | Move the focused tile left / right |
| `Super` + `Ctrl` + `↑` / `↓` | Switch to the previous / next workspace |
| `Super` + `Shift` + `↑` / `↓` | Move the focused window to the previous / next workspace |

### Window switcher (Flip 3D)

| Shortcut | Action |
| --- | --- |
| `Super` + `Tab` | Open the switcher with the previous window in front |
| `Tab` / `Shift` + `Tab` (holding `Super`) | Next / previous window |
| `←` `→` `↑` `↓`, mouse wheel, touchpad scroll, three-finger swipe | Flip through the windows |
| Release `Super`, `Return`, `Space` | Switch to the front window |
| Click a window | Switch to that window |
| `Escape` | Back to the desktop unchanged |

See [Flip 3D](#flip-3d) below. With `WINDOW_SWITCHER_STYLE = "grid"` in
`index.tsx`, `Super` + `Tab` opens the [window grid](#window-grid) instead.

### System (built into the compositor)

These are hard-wired in the compositor itself, not defined by the config — so
they keep working even if your config is broken.

| Shortcut | Action |
| --- | --- |
| `Super` + `Shift` + `R` | **Hot reload the config** — apply edits to `index.tsx` without restarting (see [Overview → Hot reload](./overview.md#hot-reload)) |
| `Super` + `Shift` + `Q` | Quit the ShojiWM session |

The same two actions are available as `shoji_wm --reload` and `shoji_wm --quit`
(see [Keybindings → Quit and reload](./keybindings-and-pointer.md#quit-and-reload)).

### Debug

| Shortcut | Action |
| --- | --- |
| `Super` + `Shift` + `F` | Toggle the FPS / frame-time overlay |

## Window appearance

The frames are Material 3 Expressive (`packages/config/src/md3e/`), in the
color scheme shoji-bar-4 sends over IPC (`theme.set`); until it does, the
Material 3 baseline dark scheme. A new scheme is moved to smoothly.

- A 36 px title bar with the app icon, the title, and minimize / maximize /
  close buttons, with 16 px rounded corners. Hovering a button grows a bubble
  from its middle; pressing squares it off; the close button turns the error
  color and the maximize icon morphs into restore when the window is
  maximized. Resting the pointer on a button shows a tooltip
  ([`<Popup/>`](./components.md#popup)).
- A thin border outside the client, white on the focused window and gray
  otherwise. When a window takes focus the border swells outward for a moment
  and settles back (`BORDER_SWELL` in `md3e/decoration.tsx`). The shadow is
  higher on the focused window.
- Clients that draw their own decorations (GTK 4, Chrome, ...) keep them, and
  get the rounded corners, the border and the shadow.
- **Fullscreen** windows drop all chrome (border, title bar, rounded corners) and
  render edge-to-edge. This bare path lets the compositor hand the buffer
  straight to the display (direct scanout) and permits tearing, for the lowest
  latency in games.

Windows open rising from below and growing, a little past their place and
back, close shrinking as they fade, and minimize sinking toward the dock
(`md3e/motion.ts`).

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
external bar (shoji-bar-4) can render workspace indicators and react to changes.

Bars that speak ext-workspace see the workspaces that have windows plus the
active one, and can activate them. They can also create a workspace by number:
`create_workspace` with the name `5` lists workspace 5 (empty) until it is
activated or gets a window, and activating it switches to it (`COMPOSITOR.workspace.event.onCreate`).

## Flip 3D

`Super` + `Tab` opens a Windows Vista style window switcher
(`packages/config/src/flip-3d.tsx`). Every window, on every workspace and
minimized ones too, lifts off the desktop into a receding diagonal stack, most
recently used in front; the bars stay on screen. Choosing a window switches to
its workspace, restores it if it was minimized, and focuses it, while the stack
settles back onto the desktop.

- Opening and closing use the window manager's easing and start and end on
  the windows' real positions, so the desktop morphs into the stack and back.
- While the switcher is open it takes all input (an
  [input grab](./keybindings-and-pointer.md#input-grab)): clicks and keys go to
  the switcher, not to the windows behind it.
- The front window gets a ring in the scheme's primary color: its decoration's
  shadow changes while it is selected (`WINDOW_SWITCHER.isSelected(id)`).
- Window blur, and the blur behind regions clients request
  (`ext-background-effect-v1`), is switched off while the switcher is open
  (each window is drawn alone into a texture there, with nothing under it to
  blur) and fades back in as the stack settles.

The stack's look (how many windows, spacing, tilt) is set by the constants at
the top of `flip-3d.tsx`. It is built from
[output composition](./output-composition.md).

## Window grid

An overview style alternative to Flip 3D (`packages/config/src/window-grid.tsx`):
set `WINDOW_SWITCHER_STYLE` in `index.tsx` to `"grid"` and `Super` + `Tab` lays
every window out side by side in a flat grid instead, most recently used first.
The grid stays open until you pick a window, so it works with the mouse alone.

| Input | Action |
| --- | --- |
| Hover a window | Select it |
| Click a window | Switch to that window |
| Click empty space, `Escape` | Back to the desktop unchanged |
| `Tab` / `Shift` + `Tab`, `←` `→` `↑` `↓`, mouse wheel | Move the selection |
| `Return`, `Space` | Switch to the selected window |
| Release `Super` after pressing `Tab` or an arrow key while holding it | Switch to the selected window (like Alt+Tab) |

Everything else — the morph from and back to the desktop, the ring on the
selected window, blur switched off while open — is shared with Flip 3D
(`window-switcher.tsx`). The grid's padding and gaps are the constants at the
top of `window-grid.tsx`.

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
- Every window gets a blurred backdrop, which shows through translucent
  clients (kitty's `background_opacity`, ...). The title bar is opaque
  (`TITLE_BAR_ALPHA` below 1 lets it show there too).
- **Layer surfaces** (bars, docks) and **layer popups** (menus) are blurred
  behind, unless a surface opts out with the `no_blur` namespace (and
  shoji-bar-4's wallpaper).

See [Effects](./effects.md) for how these are built, and
[Blurring specific windows or layers](./effects.md#blurring-specific-windows-or-layers)
for choosing which apps and bars get them.

## Drag and drop

While you drag text, files or links between windows, the dragged picture
sits on a blurred card with a pill naming what is dragged and a badge for
what a drop would do (**+** copy, **→** move, **?** ask, **×** refused), which
springs out again whenever the action changes. Over a window that accepts,
the card grows and gets a ring; over one that refuses, it shakes its head. An
accepted drop sinks into the window; one nobody accepts flies back to where
the drag started. See
[Drag and drop](./drag-and-drop.md) to change it.

## Startup programs & environment

On launch the default config:

- Sets environment variables for Wayland and the fcitx5 input method
  (`QT_QPA_PLATFORM`, `QT_IM_MODULE`, etc.).
- Starts **fcitx5** (input method), **shoji-bar-4** (the shell, from
  `~/.config/shoji-bar-4/target/release/shoji-bar-4`), and **cliphist**
  clipboard-history watchers.

These expect the corresponding programs to be installed. Remove or adjust the
`process` / `env` calls in your config if you use different tools — see
[Processes & Environment](./processes-and-env.md).

## Customizing

Your config lives at `~/.config/shojiwm/src/index.tsx`. Edit it and hot reload
with `Super` + `Shift` + `R`; the window-manager state (workspaces, tiling) is
preserved across reloads. From here,
the rest of this section documents every API the default config uses.
