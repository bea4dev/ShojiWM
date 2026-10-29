---
sidebar_position: 12
---

# Lock backgrounds and unlock overlays (draft)

This opt-in API lets a configuration select live surface roots behind a transparent
`ext-session-lock-v1` surface. It does not implement a locker, password input or
authentication. With no selection, existing opaque/blank lock behavior is unchanged.

## Selecting a background

After embedded-runtime initialization, call:

```ts
const locked = COMPOSITOR.session.configureLockBackground({
  windowIds: selectedWindowIds,
  layerIds: selectedLayerIds,
});
```

The arguments are current `WaylandWindow.id` / `WaylandLayer.id` strings. Select
them in the configuration from known client instances; refresh the selection when
those instances change. Empty arrays clear the background. At most 64 IDs are
accepted, each nonempty, at most 512 bytes, without control characters.
The return value is the compositor's actual protocol lock state. This call never
locks or unlocks the session.

Only selected root surface trees and their client subsurfaces are drawn, in
normal window/layer stacking order. Separately managed popups, SSDs, ordinary
windows and unselected layers stay hidden. There are no application names,
layer namespaces, wallpaper engines, shader files or authentication policies
built into this API. Window roots are above layer roots in the lock background.

Selections belong to the config runtime and stop applying when it stops/reloads.
No background is drawn on an output until it has a live lock surface with a
buffer. If that surface disappears, the output returns to the blank lock
fallback. Keyboard/pointer focus still follows the existing lock-only routing;
these backgrounds cannot receive lock input.

The configuration is trusted: selecting an application explicitly makes its
pixels visible while locked. Choose dedicated background clients rather than
applications containing private information. The same surface tree can contain
client-drawn UI; this is not an independent sandbox or a wallpaper decoder.

## Unlock handoff

`COMPOSITOR.effect.overlay` accepts `placement: "unlock"`. This requires an
active protocol lock, a `snapshotSource()` input, `persistent: false`, and a
`maxDuration` no greater than 3000 ms. Other overlay placements stay hidden and
are closed during lock. Paused sessions and powered-off outputs close overlays.

A locker should only request this handoff **after authentication succeeds**:

1. Launch detached overlay capture tasks while the session remains locked. Do
   not await capture from an IPC/render callback that the compositor awaits.
2. Keep the protocol lock until capture is ready. Each snapshot contains only
   the selected lock background and the lock UI, without the cursor.
3. Release the protocol lock through the locker's normal authenticated path.
4. Animate the overlay's existing shader uniforms over the live desktop.
5. Dispose the handles when finished; use their `closed` promises, native
   deadlines and a caller timeout to handle capture/output/lifecycle failure.

No built-in wave or animation is imposed. A caller can use its own shader and
reduced-motion fallback. With no live lock surface, unlock overlays do not render.
A new protocol lock closes previous overlays, and ordinary overlays cannot opt
into rendering over a lock merely by using `placement: "top"`.

## Status

Draft API extracted from a personal Quickshell desktop integration. The earlier
application-specific implementation was built and exercised locally with animated
wallpapers and a screensaver. A self-deadlock in that version was corrected by
releasing the output LayerMap guard before calling root-render helpers.

This generic implementation and its port to current upstream have been reviewed
as source, but have not been built or runtime-tested. A focused native check for
ID validation and runtime ownership is included but has not been run. Required
review includes wrong/correct authentication, locker death, lock input isolation,
reload, VT pause, DPMS, multi-output/rotation/hotplug, and snapshot cleanup.
