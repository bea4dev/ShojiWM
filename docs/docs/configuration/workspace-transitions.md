---
sidebar_position: 15
---

# Workspace wave transitions

`COMPOSITOR.workspace.transition(output, transition)` overlays a snapshot of
the previous composed output on the live desktop. A radial wave removes the
snapshot as progress increases. This optional API currently supports the TTY
backend; the nested Winit backend does not render the transition.

```ts
COMPOSITOR.workspace.transition(outputName, {
  id: "workspace-switch-1",
  progress: 0,
  direction: 1,
  accent: [0.6, 0.7, 1.0],
});
```

Use a new nonempty `id` for each switch. The first update captures the last
composed scene for that output, excluding the cursor. Subsequent updates with
the same ID reuse the snapshot. `progress` is clamped to `[0, 1]`; `1` releases
the snapshot. Positive `direction` starts the wave at the bottom, negative at
the top. `accent` contains three finite RGB components, clamped to `[0, 1]`.
The last update per output in each runtime tick wins. Invalid values throw.

This API changes rendering only. The window manager must still hide the old
workspace and show the destination. Settle the destination's tiled geometry
without a simultaneous rectangle/opacity animation before advancing the
wave: otherwise the wave can reveal a window whose own animation has not
made it visible yet. Existing configurations keep their current animations
unless they call this API.

Drive progress with an animation controller or gesture updates. To cancel a
gesture, animate progress back to zero, restore the original live workspace,
then send progress `1` to release the snapshot. On interruption, use a new ID
to capture the most recently composed scene, including a partially completed
wave. No layer-shell surface or input region is created.

Transitions are discarded on configuration reload, session pause, lock
rendering, incompatible output geometry/scale/transform, or after inactivity
(checked at 30-second intervals). Capture or shader failure falls back to
the live desktop. The wave does not refract the newly visible desktop.
