# Firefox `wayland-display` / TTY Maintenance Note

## Summary

Firefox could drive high CPU usage in both Firefox and ShojiWM on the TTY backend.

The immediate signal was that the Calloop source registered for the Wayland display fd
(`wayland-display`) woke up frequently, and ShojiWM ran TTY "maintenance"
(`space.refresh()`, `popups.cleanup()`, `flush_clients()`) too often as a result.

However, the real bug was not merely "too many wakes". The deeper problem was:

1. ShojiWM treated generic `wayland-display` readability as if maintenance was always needed.
2. Maintenance and rendering were ordered incorrectly for this backend.
3. `flush_clients()` could feed more protocol traffic back into the same loop, amplifying the
   wake frequency.

This combination created two different visible failures:

- Firefox idle / near-idle CPU stayed high because `wayland-display` readability kept causing
  expensive global maintenance work.
- A first attempt to throttle maintenance fixed CPU usage but caused a one-frame-late update bug:
  after the initial open animation ended, Kitty input no longer appeared until some other event
  (for example pointer motion) triggered another redraw.

## Why This Happened In ShojiWM But Not In Anvil/Niri

At first glance Anvil also calls `space.refresh()`, `popups.cleanup()`, and `flush_clients()` in
its udev loop every iteration, so it is tempting to conclude that ShojiWM should be fine too.
That comparison is incomplete.

ShojiWM's TTY backend has more moving parts in the redraw path:

- decoration/runtime reevaluation
- snapshot handling
- output-local redraw state
- explicit throttling / no-damage handling

Once the compositor started depending on more explicit redraw bookkeeping, the exact ordering of:

- `dispatch_clients()`
- maintenance (`space.refresh()`, `popups.cleanup()`)
- rendering
- `flush_clients()`

became observable.

In ShojiWM, running maintenance as a blanket reaction to `wayland-display` wakeups caused the
compositor to do too much global work for readable-but-not-meaningful display activity. Then, when
maintenance was simply suppressed, the loop could miss the pre-render refresh needed to make the
next committed client state render immediately.

Niri avoids this class of bug by structuring its refresh/render/flush flow much more explicitly.

## Actual Root Cause

The root cause was a combination of two design mistakes in the TTY main loop.

### 1. Wake-based maintenance gating

The event loop registered the display fd with:

- `Generic::new(display, Interest::READ, Mode::Level)`

and then maintenance was effectively treated as a consequence of the loop waking up.

That is too coarse. A readable display fd does **not** necessarily mean "run expensive global
maintenance now". It may just mean:

- more client protocol is available
- a previous flush caused follow-up traffic
- the fd remains readable under level-triggered semantics

Using the wake alone as the signal therefore overestimates "real work required".

### 2. Wrong maintenance/render ordering

The first mitigation moved maintenance behind a condition, but still ran the loop roughly like:

1. dispatch
2. maybe render
3. maybe maintenance

That broke a subtle but important case:

- a client commit gets dispatched
- the compositor now needs the refreshed scene/popups state before rendering
- but maintenance is deferred
- rendering sees stale pre-refresh state
- once the open animation stops, no further redraw arrives automatically
- the scene appears frozen until some external event, such as pointer motion, schedules a redraw

So the "frozen until cursor moves" symptom was not a separate bug. It was the same bug, seen from
the opposite side: we stopped running too much maintenance, but we had not yet made maintenance
run at the correct time.

## Fix Strategy

The fix was to stop thinking in terms of "wake source => maintenance" and instead track explicit
maintenance reasons in compositor state.

ShojiWM now records maintenance demand from events that truly require it, such as:

- new Wayland client acceptance
- actual client requests dispatched from `wayland-display`
- libinput activity
- runtime scheduler actions that affect scene state

Then the TTY loop performs:

1. dispatch
2. pre-render maintenance, but only if there is a pending reason or a periodic idle sweep is due
3. render if redraw is needed
4. flush clients only after render or maintenance actually ran

This preserves the important pre-render refresh while avoiding unconditional maintenance on every
`wayland-display` wake.

## Why The Final Version Works

The final version fixes both observed failures:

- Firefox no longer causes high CPU simply because the display fd remains readable.
- Kitty no longer gets "stuck after the open animation" because the pre-render refresh still runs
  when a real maintenance reason exists.

In short:

- wake frequency is no longer treated as proof that maintenance is required
- maintenance is run for explicit reasons
- the ordering is now maintenance before render, not after

## Useful Debugging Knob

Set:

```bash
SHOJI_TTY_MAINTENANCE_DEBUG=1
```

This logs:

- whether a redraw was pending
- whether maintenance was pending
- which reasons requested maintenance
- how many Wayland requests were actually dispatched
- which event sources woke the loop

This is useful if a future change reintroduces either:

- `wayland-display` wake amplification
- "state only updates after pointer motion" style bugs
