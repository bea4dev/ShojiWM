---
sidebar_position: 12
---

# Lock backgrounds and unlock overlays

While the session is locked, ShojiWM normally draws nothing but the locker's
`ext-session-lock-v1` surfaces. This opt-in API lets a configuration keep chosen
windows or layer-shell surfaces visible **behind** a transparent lock surface,
such as a live wallpaper or a screensaver client. It can also hand the lock screen
over to the desktop with an animated overlay after a successful unlock.

The API does not implement a locker, password input or authentication. With no
selection, the lock behaves exactly as before.

## Selecting a background

```ts
const locked = COMPOSITOR.session.configureLockBackground({
  windowIds: [wallpaperWindow.id],
  layerIds: [wallpaperLayer.id],
});
```

- The arguments are current `WaylandWindow.id` / `WaylandLayer.id` strings.
  Pick them from known client instances in the configuration (for example in
  `COMPOSITOR.event.onOpen`), and call again when those instances change.
  Empty arrays clear the background.
- At most 64 IDs are accepted, each nonempty, at most 512 bytes and without
  control characters. Unknown fields are rejected.
- The return value is whether the session is locked right now. The call itself
  never locks or unlocks the session.
- The selection may change at any time, including while locked; it applies from
  the next frame. Call it after the config has loaded (from an event, timer or
  `onEnable`), not at module scope.

What is drawn:

- Only the selected root surface trees and their client subsurfaces. Separately
  managed popups, SSDs, effects, unselected windows and unselected layers stay
  hidden. Windows are drawn at their client rectangle, without the window's
  composition.
- Stacking follows the normal window and layer order, but every selected window
  is above every selected layer.
- Input does not change: keyboard and pointer still reach only the locker.

When it stops:

- Selections belong to the config runtime. They are dropped when it stops or
  reloads, so select again from `onEnable` if needed.
- Nothing is drawn on an output until it has a lock surface with a buffer. If the
  locker dies, the output returns to the blank lock screen, backgrounds included.

:::warning
The configuration is trusted: selecting a client makes its pixels visible while
locked, including any UI it draws in that surface tree. Select dedicated
background clients, not applications that may show private information. This is
not a sandbox.
:::

## Unlock handoff

`COMPOSITOR.effect.overlay` accepts `placement: "unlock"`. It is the only kind of
overlay that can be created while locked, and it stays over the desktop after
the locker unlocks. Creating one requires:

- an active lock,
- a `snapshotSource()` input,
- `persistent: false`,
- a `maxDuration` of at most 3000 ms.

Other placements are closed when the session locks and cannot be created during
it. A new lock closes previous overlays. A paused session (VT switch) and a
powered-off output close overlays as well. With no live lock surface, unlock
overlays are not drawn.

The snapshot holds the lock scene: the lock surface and the selected background,
without the cursor. Where neither covers the output, the snapshot is
**transparent**, so the live desktop shows through those areas as soon as the
session unlocks. Let the background cover the whole output, or return an opaque
color from the shader (for example `vec4(frozen.rgb, 1.0)`).

Only start the handoff **after authentication succeeds**:

1. Create the overlay from a detached task while the session is still locked.
   Do not await it from a callback the compositor awaits (see
   [Output overlays](./output-overlays.md)).
2. Wait until the overlay is ready (its promise resolves), so the snapshot has
   been taken.
3. Unlock through the locker's normal authenticated path.
4. Animate the shader's uniforms over the live desktop.
5. Dispose the handle when finished. Handle failure with its `closed` promise,
   the native deadline and a timeout of your own.

```ts
async function revealDesktop(output: string, unlock: () => void) {
  const progress = signal(0);
  const handle = await COMPOSITOR.effect.overlay(output, {
    placement: "unlock",
    maxDuration: 3_000,
    effect: compileOverlayEffect({
      input: snapshotSource(),
      alpha: "preserve",
      pipeline: [shaderStage(dissolve, { uniforms: { progress } })],
    }),
  });
  try {
    unlock(); // tell the locker to release the lock
    const start = Date.now();
    while (progress.value < 1) {
      progress.value = Math.min(1, (Date.now() - start) / 600);
      await new Promise<void>((resolve) => setTimeout(resolve, 8));
    }
  } finally {
    handle.dispose();
  }
}
```

No animation is built in. Bring your own shader and a reduced-motion fallback.

## Rust

The Rust SDK has the same API:

```rust
COMPOSITOR.session.configure_lock_background(LockBackground {
    window_ids: vec![window.id()],
    layer_ids: vec![],
})?;
let overlay = Overlay::new(effect)
    .placement(OverlayPlacement::Unlock)
    .max_duration_ms(3_000.0);
```

## Status

This API is new. The selection, locker death, reload and handoff behavior have
been checked in a nested (Winit) session. A real TTY session with a full locker,
VT switching, DPMS and multiple or rotated outputs has not been tested yet.
