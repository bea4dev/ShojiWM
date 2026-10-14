---
sidebar_position: 4.5
---

# Cursor

`COMPOSITOR.cursor` configures the XCursor theme used by ShojiWM's
compositor-owned named cursors, including the default pointer and SSD resize
cursors. It also [moves the cursor](#moving-the-cursor),
[overrides its shape](#overriding-the-cursor-shape) and lets you
[ignore app pointer locks](#ignoring-app-pointer-locks).

```ts
COMPOSITOR.cursor.configure({
  theme: 'Bibata-Modern-Classic',
  size: 24,
});
```

`theme` is the installed XCursor theme name. `size` is an integer from 1 to 512
in **logical pixels**. ShojiWM chooses an appropriately scaled image for each
output, so a size of 24 remains the same logical size on scale-1 and scale-2
outputs.

Calling `configure()` takes effect without restarting ShojiWM and also:

- sets `XCURSOR_THEME` and `XCURSOR_SIZE` for processes started afterwards;
- publishes those variables to the systemd and D-Bus activation environments;
- discards ShojiWM's decoded cursor and GPU buffer caches; and
- schedules the new named cursor for the next redraw.

## Reloading theme files

If theme files were replaced without changing their theme name or size, reload
the current theme explicitly:

```ts
COMPOSITOR.cursor.reload();
```

This is also available from a keybinding:

```ts
COMPOSITOR.key.bind('reload-cursor', 'Super+Shift+C', () => {
  COMPOSITOR.cursor.reload();
});
```

## Client-owned cursor surfaces

Wayland clients may send their own cursor surface. ShojiWM must display that
surface as supplied by the client and cannot replace it with a named cursor.
The exported XCursor environment variables let newly started toolkits choose
the same theme, but an already-running application may need to be restarted or
to reload its own settings before its client-owned cursor changes.

Changing the cursor configuration never replaces or modifies a client-owned
cursor surface.

## Moving the cursor

`setPosition` moves the cursor, either to global logical coordinates or to a
point of an output. The output form places the point the same way as
[pointer regions](./keybindings-and-pointer.md#pointer-regions): `anchor`
(default `"center"`), then `offset` (x right, y down).

```ts
COMPOSITOR.cursor.setPosition({x: 800, y: 500});
COMPOSITOR.cursor.setPosition({output: 'DP-1', anchor: 'center'});
COMPOSITOR.cursor.setPosition({output: 'DP-1', anchor: 'top-right', offset: {x: -40, y: 40}});

const position = COMPOSITOR.cursor.getPosition();
// { x, y, outputName } — undefined until the pointer has moved once
```

- Clients see an ordinary motion: hover and pointer focus follow the cursor,
  and pointer regions report `enter` / `leave`. `onPointerMove` does **not**
  fire, so a listener that moves the cursor cannot call itself in a loop.
- A point outside every output is moved to the nearest output. An output form
  stays on the named output.
- It is ignored while a client holds the pointer locked or confined (a game),
  while a pointer button is held (a drag), and while the screen is locked.
- `getPosition()` returns the position as of the last pointer motion without
  asking the compositor, so it is cheap to call anywhere.

```ts
// Move the cursor to the centre of the next output
COMPOSITOR.key.bind('cursor-next-output', 'Super+Ctrl+Right', () => {
  const outputs = COMPOSITOR.output.list;
  const current = COMPOSITOR.cursor.getPosition()?.outputName;
  const next = outputs[(outputs.indexOf(current ?? '') + 1) % outputs.length];
  if (next) COMPOSITOR.cursor.setPosition({output: next});
});
```

Only your config can move the cursor. Wayland clients have no way to do so,
and this API is not exposed to them.

## Overriding the cursor shape

`overrideKind(kind)` shows the named cursor everywhere, whatever the window
under the pointer asks for, until it is called again. `"none"` hides the
cursor, and `null` gives the cursor back to the windows.

```ts
COMPOSITOR.cursor.overrideKind('no-drop'); // show the "no drop" cursor
COMPOSITOR.cursor.overrideKind('none');    // hide the cursor
COMPOSITOR.cursor.overrideKind(null);      // back to normal
```

Names are CSS `cursor` values (`"default"`, `"pointer"`, `"text"`,
`"grabbing"`, `"copy"`, `"not-allowed"`, `"ew-resize"`, ...) and are looked
up in the configured theme. The override also wins over the resize cursors of
window borders.

A typical use is [during a drag](./drag-and-drop.md#changing-the-cursor-during-a-drag).
In a nested (windowed) session the host draws the cursor, so the override
has no visible effect there.

## Ignoring app pointer locks

Games and other apps can lock the cursor in place or confine it to their
window. While they hold it, the cursor cannot leave the window, so you cannot
drag the game with `Super` or reach a bar on another output without quitting
the game first.

`bindAppLockIgnoreModifier(modifier)` ignores those locks while `modifier` is
held. The default config uses `Super`:

```ts
COMPOSITOR.cursor.bindAppLockIgnoreModifier('Super');
```

Hold `Super`, and the cursor is free; with `Super` still held you can also
drag the game window. Release it over the game and the lock comes back.

To switch it on and off yourself, use `setAppLockIgnored`:

```ts
COMPOSITOR.cursor.setAppLockIgnored(true);   // ignore app locks
COMPOSITOR.cursor.setAppLockIgnored(false);  // respect them again
```

While locks are ignored:

- **The app is told its lock was released.** Most games (SDL and similar) keep
  their lock and take it back on their own. A lock made for one use only, such
  as a browser's pointer lock, is gone, just as when the cursor leaves the
  window; click in the page again to lock it.
- **The cursor appears where the app wants it.** Apps can say where the cursor
  should show up when the lock ends (the crosshair, for instance); ShojiWM
  follows that hint.
- **The app stops receiving raw mouse motion**, so a game's camera does not
  turn while you move the cursor across it.
- **Over that app, ShojiWM shows its default arrow** instead of the app's own
  cursor. Games usually hide the cursor while they lock it, so this is how you
  can see that it is free. Elsewhere the cursor looks as usual.

The modifier itself still reaches the focused app as a key, as it does for
`Super` + drag.
