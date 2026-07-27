---
sidebar_position: 7
---

# Window composition

`COMPOSITOR.window.composition` is the heart of ShojiWM's customization. Assign a
function that, given a window, returns a TSX tree describing how that window is
placed and decorated. The compositor calls it for every toplevel window and
re-runs it (incrementally) whenever a value it read changes.

```tsx
COMPOSITOR.window.composition = (window) => (
  <ManagedWindow rect={window.position} zIndex={1}>
    <WindowBorder
      style={{
        borderRadius: 10,
        border: {
          px: 2,
          color: window.isFocused((f) => (f ? "#d7ba7d" : "#4f5666")),
        },
      }}
    >
      <Box direction="column">
        <Box
          direction="row"
          style={{ height: 28, paddingX: 8, gap: 8, alignItems: "center" }}
        >
          <AppIcon icon={window.icon} style={{ width: 16, height: 16 }} />
          <Label text={window.title} style={{ flexGrow: 1, fontSize: 13 }} />
        </Box>
        <ClientWindow />
      </Box>
    </WindowBorder>
  </ManagedWindow>
);
```

Every tree must contain exactly one [`<ManagedWindow/>`](#managedwindow) wrapping
exactly one [`<ClientWindow/>`](#clientwindow). Everything between them — borders,
title bars, buttons — is your decoration, built from the
[SSD components](./components.md).

## The `window` object

The argument is a `WaylandWindow`: a live, reactive handle to one window. Reading
its signals inside composition automatically subscribes you to changes.

### Reactive properties

Each is a `ReadonlySignal` — read it as `window.title()` or `window.title.value`,
or map it as `window.isFocused((f) => f ? 'a' : 'b')`.

| Property          | Type                      | Meaning                                      |
| ----------------- | ------------------------- | -------------------------------------------- |
| `title`           | `string`                  | Window title                                 |
| `appId`           | `string \| undefined`     | Application id (e.g. `"org.gnome.Nautilus"`) |
| `icon`            | `WindowIcon \| undefined` | Application icon                             |
| `isFocused`       | `boolean`                 | Holds keyboard focus                         |
| `isFloating`      | `boolean`                 | Floating (non-tiled)                         |
| `isMaximized`     | `boolean`                 | Maximized                                    |
| `isFullscreen`    | `boolean`                 | Fullscreen                                   |
| `decoration`      | `WindowDecorationState`   | Effective CSD/SSD negotiation state          |
| `isResizable`     | `boolean`                 | Client allows interactive resize             |
| `isTransient`     | `boolean`                 | A child (dialog) of another window           |
| `parentId`        | `string \| undefined`     | Parent window id, if transient               |
| `sizeConstraints` | `WindowSizeConstraints`   | Min/max size from the client                 |
| `interaction`     | snapshot                  | Current pointer/drag interaction state       |

Non-reactive helpers: `id` (stable string), `position` / `rect` (current logical
geometry), `state` (per-window store — see [State & Signals](./state-and-signals.md)),
`transform` (GPU transform), `animation` (see [Animations](./animations.md)).

### Client-side and server-side decorations

Wayland applications can draw their own title bar and borders (client-side
decoration, CSD), or ask the compositor to draw them (server-side decoration,
SSD). Configure ShojiWM's policy once, then use the negotiated result in the
composition:

```tsx
COMPOSITOR.window.decoration.configure((window, context) => {
  const appId = (window.appId() ?? "").toLowerCase();

  // Preserve the CSD baseline until metadata identifies the application.
  if (appId.length === 0) {
    return { mode: context.clientPreference ?? "client" };
  }
  if (appId.includes("firefox")) {
    return { mode: "client" };
  }
  return { mode: "server" };
});

COMPOSITOR.window.composition = (window) => {
  const decoration = window.decoration();
  const useClientDecoration =
    decoration.mode === "client" &&
    !(
      decoration.clientPreference === "server" &&
      decoration.configuredMode === "server"
    );

  return (
    <ManagedWindow rect={window.position} zIndex={1}>
      {useClientDecoration ? (
        <ClientWindow />
      ) : (
        <WindowBorder style={{ border: { px: 2, color: "#4f5666" } }}>
          <Box direction="column">
            {/* title bar */}
            <ClientWindow />
          </Box>
        </WindowBorder>
      )}
    </ManagedWindow>
  );
};
```

The resolver runs synchronously when a decoration object is created, the
client changes its preference, relevant metadata changes, or the TS config is
reloaded. `context` contains:

The legacy KDE decoration manager advertises CSD as its global default because
that event is sent before per-window metadata exists. The app id may therefore
still be empty during the first decoration request. Preserve the CSD baseline
while it is empty, then apply the final per-app policy when metadata arrives,
as in the example above. Sending an early SSD response can make clients such
as Firefox or Chromium construct reduced window chrome that they do not fully
rebuild after a later CSD response. The resolver must be side-effect free;
window actions such as `focus()` are rejected in this context.

| Property           | Meaning                                                             |
| ------------------ | ------------------------------------------------------------------- |
| `protocol`         | `xdg-decoration-v1`, `kde-server-decoration`, `xwayland`, or `none` |
| `clientPreference` | The client's requested mode, or `null` if it did not choose one     |
| `canNegotiate`     | Whether ShojiWM can send the decision through a decoration protocol |
| `reason`           | Why the policy is being evaluated                                   |

`window.decoration.configuredMode` is the last mode ShojiWM selected.
`window.decoration.mode` is the effective mode acknowledged and committed by
the client; use this value as the normal rendering baseline. The two can differ
briefly during an XDG configure/ack/commit cycle. When both
`clientPreference` and `configuredMode` are `server`, the client and compositor
already agree on SSD, so the example renders SSD before the effective mode
catches up. This avoids an initial undecorated frame with toolkits that defer
their configure commit until activation, without forcing SSD onto a client that
requested CSD.

When `canNegotiate` is `false`, ShojiWM can still select which composition to
draw, but cannot force the client to add or remove CSD. XWayland decoration is
also observable as `protocol: 'xwayland'`, but is not negotiated through these
Wayland protocols.

For the legacy KDE protocol, ShojiWM suppresses repeated acknowledgements to
prevent a client/compositor renegotiation loop. If eight identical requests
arrive, it writes one warning to the compositor log for that loop; it does not
spam one warning per request. XDG decoration requests always receive the
configure response required by that protocol.

### Methods

| Method                            | Effect                                             |
| --------------------------------- | -------------------------------------------------- |
| `close()`                         | Ask the client to close                            |
| `maximize()` / `unmaximize()`     | Toggle maximize                                    |
| `minimize()`                      | Minimize                                           |
| `fullscreen()` / `unfullscreen()` | Toggle fullscreen                                  |
| `focus()`                         | Give keyboard focus and raise                      |
| `scheduleAnimation(options)`      | Animate managed-window geometry                    |
| `cancelAnimation(channel?)`       | Cancel a running animation                         |
| `setCloseAnimationDuration(ms)`   | Delay surface destruction to fit a close animation |
| `isXWayland()`                    | `true` if running under XWayland                   |

## ManagedWindow

`<ManagedWindow/>` is the anchor that binds a window into the layout system.
Place one per window.

| Prop             | Type                     | Meaning                                                  |
| ---------------- | ------------------------ | -------------------------------------------------------- |
| `rect`           | `ManagedWindowRect`      | Logical `{x, y, width, height}` of the window            |
| `zIndex`         | `number`                 | Stacking order (higher is on top)                        |
| `workspace`      | `string \| number`       | Workspace assignment                                     |
| `visibleOutputs` | `string[] \| null`       | Restrict to named outputs (`null` = all)                 |
| `visible`        | `boolean`                | Show/hide without unmapping                              |
| `idle`           | `boolean`                | Exclude from focus cycling; treat as background          |
| `interactive`    | `boolean`                | When `false`, ignore pointer input                       |
| `forceRectSize`  | `boolean`                | Force the client to `rect`'s size                        |
| `tiled`          | `boolean`                | Send the tiled state to the client                       |
| `opacity`        | `number`                 | `0.0`–`1.0`                                              |
| `transform`      | `ManagedWindowTransform` | Extra GPU transform                                      |
| `allowTearing`   | `boolean`                | Permit tearing while fullscreen + direct-scanout (games) |

All props accept signals for reactive layout. `rect`, `zIndex`, etc. are usually
driven by your window-manager logic.

## ClientWindow

`<ClientWindow/>` renders the client's actual surface buffer. A leaf node — no
children. Alias: `<Window/>`.

```tsx
<ClientWindow />
```

A bare client window preserves the complete client-owned surface tree, including
CSD shadows and transparent resize margins outside `xdg_surface.window_geometry`.
It is not clipped merely because it occupies the managed window slot.

Clipping is owned by the surrounding SSD hierarchy. Wrap the client in a
container with a `border` to clip descendants to that border's inner edge (and
rounded shape), or use `overflow: "hidden"` for an explicit clip. Set
`overflow: "visible"` on a bordered container to opt out.

```tsx
<WindowBorder
  style={{ border: { px: 2, color: borderColor }, borderRadius: 8 }}
>
  <ClientWindow />
</WindowBorder>
```

:::tip[Fullscreen fast path]
For fullscreen windows, return **only** a bare `<ClientWindow/>` inside
`<ManagedWindow/>` (no border, no title bar). Rendering nothing else is what lets
the TTY backend promote the client buffer to the primary plane (direct scanout)
for the lowest latency. The default config does exactly this.
:::
