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
