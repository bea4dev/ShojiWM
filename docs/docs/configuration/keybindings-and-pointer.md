---
sidebar_position: 5
---

# Keybindings & Pointer

## Keyboard shortcuts

`COMPOSITOR.key.bind(id, shortcut, handler, options?)` registers a
compositor-level keyboard or mouse-wheel shortcut.

```ts
COMPOSITOR.key.bind('terminal', 'Super+T', () => {
  COMPOSITOR.process.spawn({command: ['kitty']});
});
```

| Argument | Type | Meaning |
| --- | --- | --- |
| `id` | `string` | Unique name (shown in help UIs). Re-binding the same id replaces it. |
| `shortcut` | `string` | Modifier+key notation, e.g. `"Super+Shift+Left"` |
| `handler` | `() => void` | Called when the shortcut fires |
| `options` | `{on?: "press" \| "release"}` | When to fire — defaults to `"press"` |

### Shortcut syntax

Combine modifiers and a key with `+`:

- **Modifiers:** `Super`, `Ctrl`, `Shift`, `Alt`
- **Keys:** letters (`T`, `Q`), arrows (`Left`, `Right`, `Up`, `Down`),
  function keys (`F`), etc.
- **Mouse wheel:** `WheelScrollUp`, `WheelScrollDown`, `WheelScrollLeft`,
  `WheelScrollRight`. Wheel shortcuts fire on discrete mouse-wheel events;
  touchpad scrolling remains continuous and is forwarded normally. Wheel
  shortcuts use the default `on: "press"` phase.

```ts
COMPOSITOR.key.bind('close', 'Super+Q', () => focused?.close());
COMPOSITOR.key.bind('move-tile-left', 'Super+Shift+Left', () => moveTile(-1));
COMPOSITOR.key.bind('screenshot', 'Super+P', () => {
  COMPOSITOR.process.spawn({command: 'hyprshot -m region --raw | swappy -f -'});
});
COMPOSITOR.key.bind('focus-left', 'Super+WheelScrollUp', () => focusTile(-1));
COMPOSITOR.key.bind('focus-right', 'Super+WheelScrollDown', () => focusTile(1));
```

### Tap bindings (`on: "release"`)

Binding a bare modifier with `{on: "release"}` makes it a **tap** — it fires when
the key is released, but only if no other key/button was pressed in between. The
default config uses this to open a launcher on a quick `Super` tap, while still
allowing `Super` to act as a modifier for other shortcuts.

```ts
COMPOSITOR.key.bind('launcher-tap', 'Super', openLauncher, {on: 'release'});
```

### Quit and reload

`Super+Shift+Q` (quit) and `Super+Shift+R` (reload the config) are built into
the compositor and always work, even while the config is broken. The same two
actions are available from the command line, so a status bar, a script or a
binding of your own can trigger them:

```sh
shoji_wm --quit     # or -q
shoji_wm --reload   # or -r
```

```ts
COMPOSITOR.key.bind('reload', 'Super+Ctrl+R', () => {
  COMPOSITOR.process.spawn({command: ['shoji_wm', '--reload']});
});
```

The command talks to the running compositor over the socket named by
`SHOJIWM_SOCKET`, which ShojiWM exports to everything it starts (and to the
systemd/D-Bus activation environment). Without it, the socket is derived from
`WAYLAND_DISPLAY`.

## Pointer

`COMPOSITOR.pointer` configures mouse interactions handled by the compositor
itself. (For acceleration, scroll method, and other per-device tuning, use
[`COMPOSITOR.input`](./input.md).)

### Move windows with a modifier

`bindWindowMoveModifier(modifier)` lets the user drag any window by holding the
modifier and clicking anywhere on it — no need to grab the title bar.

```ts
COMPOSITOR.pointer.bindWindowMoveModifier('Super');
```

:::tip
Interactive resize hit areas are configured per-window via the
`<WindowBorder interaction={{resizeHitArea: …}}>` prop — see
[SSD Components](./components.md#windowborder).
:::

### Hot corners

`bindHotCorner(id, corner, handler, options?)` runs `handler` when the pointer
reaches a corner of the screen. The compositor watches the corner itself, so
moving the pointer anywhere else costs nothing.

```ts
COMPOSITOR.pointer.bindHotCorner('overview', 'top-left', () => {
  openWindowSwitcher();
});
```

| Option | Default | Meaning |
| --- | --- | --- |
| `output` | every output | Only this output's corner |
| `size` | `1` | Side of the square corner region, logical px |
| `delay` | `0` | Stay this many ms in the corner before `handler` runs |
| `pressure` | — | Run once the pointer is pushed this many px into the corner instead of on entering it |
| `leaveMargin` | `8` | The pointer has to move this far away before the corner can fire again |
| `ignoreWhenFullscreen` | `true` | Inactive on an output showing a fullscreen window |
| `ignoreWhileButtonHeld` | `true` | Inactive while dragging |

Only real corners count: where another monitor continues past an edge, the
pointer just crosses into it, so that corner never fires. Binding the same `id`
again replaces the corner; the returned function removes it.

`pressure` is the way to avoid triggering a corner by accident. The cursor stops
at the edge of the screen, but the compositor still sees the mouse moving, so
"keep pushing 100 px into the corner" is something you do on purpose:

```ts
COMPOSITOR.pointer.bindHotCorner('overview', 'top-left', openOverview, {pressure: 100});
```

### Pointer regions

Hot corners are built on pointer regions: rectangles placed relative to an
output that report the pointer entering and leaving them. Define one with
`defineRegion(id, options)` and receive its events with
`COMPOSITOR.event.onPointerRegion`.

```ts
// A 400 × 2 px strip at the top centre of every output
COMPOSITOR.pointer.defineRegion('top-strip', {
  anchor: 'top',
  size: {width: 400, height: 2},
  outerEdgesOnly: true,
});

COMPOSITOR.event.onPointerRegion((event) => {
  // { id, phase: "enter" | "leave" | "pressure", outputName, position, modifiers, timestamp }
  if (event.id === 'top-strip' && event.phase === 'enter') {
    showBar(event.outputName);
  }
});
```

| Option | Default | Meaning |
| --- | --- | --- |
| `size` | (required) | `{width, height}` in logical px. The part outside the output is cut off. |
| `anchor` | `"top-left"` | `"top-left"`, `"top"`, `"top-right"`, `"left"`, `"center"`, `"right"`, `"bottom-left"`, `"bottom"`, `"bottom-right"`. The region's point of that name is placed on the output's. |
| `offset` | `{x: 0, y: 0}` | Moves the region after anchoring (x right, y down) |
| `output` | every output | Only this output; otherwise each output gets its own copy |
| `outerEdgesOnly` | `false` | Disable the region on an output where an edge it touches continues into another output |
| `leaveMargin` | `0` | Once inside, the region grows by this many px before the pointer counts as leaving |
| `pressure` | — | Report `pressure` once the pointer, inside the region, has been pushed this many px against the edge of the screen (once per visit) |
| `ignoreWhenFullscreen` | `false` | Inactive on an output showing a fullscreen window |
| `ignoreWhileButtonHeld` | `false` | Inactive while a pointer button is held |

- **Placement follows the outputs.** Regions are given relative to an output,
  so they stay in place across hotplug, resolution and scale changes without
  being defined again.
- **Events come in order.** A single motion can leave one region and enter
  another; every `leave` is reported before any `enter`. Moving from one
  output's copy of a region straight into another's is a `leave` followed by
  an `enter`, each with its own `outputName`.
- **Regions are checked when the pointer moves.** A window going fullscreen
  under a still pointer reports nothing until the pointer moves again.
- **Redefining keeps the state.** `defineRegion` with an existing id replaces
  the region without a fresh `enter`. `removeRegion(id)` removes it silently
  (no `leave`).
- `onPointerRegion` runs synchronously, like key bindings: window actions in
  the handler take effect right away. Keep it short.

## Input grab

`COMPOSITOR.input.grab(handlers)` takes all input until the returned grab is
released: keys, pointer buttons, scrolling and swipes go to your handlers
instead of the clients. It is for modal UIs the compositor draws itself, such
as the default config's [Flip 3D](./default-config.md#flip-3d) switcher.

```ts
const grab = COMPOSITOR.input.grab({
  onKey(event) {
    // { key: "Escape", keycode, state: "pressed" | "released", modifiers, timestamp }
    if (event.key === "Escape" && event.state === "pressed") grab.release();
  },
  onPointerMotion(event) {},  // { position, delta, outputName, modifiers }
  onPointerButton(event) {},  // { button, buttonName: "left" | ..., state, position, outputName }
  onScroll(event) {},         // { deltaX, deltaY, discreteX, discreteY, source, position }
  onSwipe(event) {},          // a GestureSwipeEvent
  onCancel(reason) {},        // "sessionLock" | "error" | "replaced"
});
```

- **Everything goes to the grab.** Key bindings do not fire while it is held,
  except the compositor's own (`Super` + `Shift` + `R` / `Q`, `Ctrl` + `Alt` +
  `F1`–`F12`). Positions are global logical pixels.
- **The pointer still moves the cursor.** Clients lose pointer focus for the
  duration (hover states clear) and get it back when the grab ends.
- **Keys held when the grab starts** deliver their release to the client that
  saw the press, as well as to the grab, so a modifier held to open the grab
  (`Super` in `Super` + `Tab`) is not left stuck in the focused window. Its
  release is also how a "hold to keep open" UI notices the user let go.
- **Key names** are xkb keysym names, as in shortcuts: `"Tab"`, `"Return"`,
  `"space"`, `"Escape"`, `"Left"`, `"Super_L"`, `"a"`.
- **The compositor ends the grab on its own** when the screen locks, when a
  handler throws, and on hot reload, so a broken config cannot keep the desktop
  unreachable. `onCancel` tells you (not on reload: that config is gone).
  Starting another grab replaces the current one, whose `onCancel` gets
  `"replaced"`.
- Touch input is not grabbed.

Calls that act on windows (`window.focus()`, activating a window through your
window manager) work in the handlers as they do in key bindings. Inside a 3D
layout, [`pickPlane`](./output-composition-reference.md#picking) finds the
plane under the pointer.
