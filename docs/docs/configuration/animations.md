---
sidebar_position: 11
---

# Animations

ShojiWM offers **two ways** to animate, with different trade-offs:

| Approach | Where it runs | Best for |
| --- | --- | --- |
| **Signal-driven** (`window.animation` + `animationVariable`) | The TypeScript runtime | Decoration/chrome: hover, focus, GPU transforms, anything you map into arbitrary TSX |
| **Compositor-driven** (`window.scheduleAnimation(...)`) | The Rust core | Managed-window geometry/opacity: open, close, minimize, move, resize, workspace switches |

The difference in one line: with the **signal** approach *you* compute each
frame's value in TS and the composition re-runs; with **`scheduleAnimation`** you
describe the whole animation once and **Rust plays it back**, applying the result
as a delta on top of the layout — no per-frame TS work.

See [Which should I use?](#which-should-i-use) at the bottom for guidance.

---

## Signal-driven animations

These are driven by **animation variables** — named tokens whose value smoothly
interpolates over time. You read a variable as a signal and feed it into
transforms, opacity, or any other style; you start/stop it from event handlers.
Each window keeps its own progress per variable.

### Animation variables

Create a token once at module scope with `animationVariable(debugName?)`, then
use it through `window.animation`:

```ts
import {animationVariable, milliseconds, seconds} from 'shoji_wm';

const open = animationVariable('open');

COMPOSITOR.event.onOpen((window) => {
  window.animation.start(open, {duration: seconds(0.18), from: 0, to: 1});
});

COMPOSITOR.event.onFocus((window, focused) => {
  window.animation.start(open, {duration: milliseconds(120), to: focused ? 1 : 0});
});
```

`milliseconds(n)` and `seconds(n)` are readability helpers — both just return a
millisecond number.

### The animation controller

`window.animation` (an `AnimationController`) exposes:

| Method | Purpose |
| --- | --- |
| `variable(v)` | Read the variable's progress as a `ReadonlySignal<number>` |
| `signal(v)` | Alias for `variable` |
| `start(v, options)` | Start/restart an animation |
| `stop(v)` | Stop, keeping the current value |
| `set(v, value)` | Jump to a value, cancelling any running task |
| `running(v)` | `true` while an animation is active |

`start` options (`AnimationStartOptions`):

| Option | Type | Meaning |
| --- | --- | --- |
| `duration` | `number` (ms) | Total time |
| `from` | `number` | Start value (defaults to the current value — smooth retargeting) |
| `to` | `number` | Target value (defaults to `1`) |
| `easing` | `(t: number) => number` | Easing applied to `0..1` progress |
| `repeat` | `"loop" \| "ping-pong"` | Repeat behavior |

Omitting `from` makes direction changes and retargeting smooth — the animation
continues from wherever the value currently is.

`start` returns a `Promise<boolean>`: `true` once the value reaches `to`,
`false` if `start`, `stop` or `set` cut it short (a repeating animation only
ends that way). Use it to clean up after an animation:

```ts
drag.animation.start(fade, {to: 0, duration: 150}).then((done) => {
  if (done) view.dispose();
});
```

### Reading a variable in composition

`variable(v)` returns a signal you can map into a style. Reading it inside
composition makes the decoration update every frame the animation advances.

```tsx
COMPOSITOR.window.composition = (window) => {
  const t = window.animation.variable(open);
  const scale = t((x) => 0.8 + x * 0.2); // 0.8 → 1.0
  window.transform.scaleX = scale;
  window.transform.scaleY = scale;
  window.transform.opacity = t;
  return (/* … */);
};
```

Because the value lives in a signal that composition reads, each frame triggers a
(targeted) re-evaluation. That flexibility is the point — but it also means this
path costs TS work per frame, so reserve it for chrome, not for the heavy
geometry animations below.

---

## Compositor-driven animations: `scheduleAnimation`

`window.scheduleAnimation(options)` hands a complete animation description to the
Rust core, which interpolates it **every frame on its own** and applies the
result to the managed window. The TS runtime is not involved per frame — there is
no re-composition and no per-frame IPC — so this is the lightweight path for the
frequent, heavy transitions (open/close/minimize/move/resize/workspace).

```ts
window.scheduleAnimation({
  channel: 'open',
  rect: {
    from: {x: 0, y: 200, width: 0, height: 0},
    to:   {x: 0, y: 0,   width: 0, height: 0},
    duration: 500,
    easing: {kind: 'cubicBezier', x1: 0.2, y1: 0, x2: 0, y2: 1},
    mode: 'add',
  },
  opacity: {from: 0, to: 1, duration: 500, mode: 'multiply'},
});
```

### What you can animate

`ManagedWindowScheduleAnimationOptions` has up to three independent properties,
plus a channel:

| Field | Animates | Option type |
| --- | --- | --- |
| `rect` | The window's `{x, y, width, height}` | `ManagedWindowRectAnimationOptions` |
| `offset` | A positional `{x, y}` offset | `ManagedWindowPointAnimationOptions` |
| `opacity` | A scalar opacity | `ManagedWindowScalarAnimationOptions` |
| `channel` | *(string)* groups the animation — see [Channels](#channels-and-cancellation) | — |

Each of `rect` / `offset` / `opacity` takes the same shape:

| Option | Type | Meaning |
| --- | --- | --- |
| `to` | value | Target (required) |
| `from` | value | Start (optional — defaults to the current value) |
| `duration` | `number` (ms) | Total time |
| `easing` | easing | See [Easing](#easing) (default linear) |
| `mode` | `"override" \| "add" \| "sub" \| "multiply"` | How it combines with the base — see below |

For `rect` and `offset` the value is `{x, y, …}`; for `opacity` it is a number.
`mode` on `rect`/`offset` may **not** be `"multiply"`.

### Modes: how the animation combines with the layout

This is the key to `scheduleAnimation`. The animated value does not blindly
replace the window's state — it is **combined** with the base value that your
window manager is computing live, according to `mode`:

| Mode | Result |
| --- | --- |
| `"override"` | `animated` — replace the base value |
| `"add"` | `base + animated` — add a delta on top of the layout |
| `"sub"` | `base - animated` |
| `"multiply"` | `base × animated` (opacity only) |

`add` is what makes these animations **ride along with the layout**. In the
open example above, `rect.mode: 'add'` animates a `+200px` vertical offset that
decays to `0` — so the window slides up into place *relative to wherever the
tiling/floating layout currently puts it*. If the layout shifts mid-animation
(another window opens, a tile resizes), the slide still lands correctly because
Rust adds the animated delta to the live base rect each frame. `multiply` on
opacity lets a fade compose with a window whose base opacity is already changing.

### Easing

`easing` accepts:

- `"linear"` (the default) or `{kind: "linear"}`
- `{kind: "cubicBezier", x1, y1, x2, y2}` — a CSS-style cubic-bézier curve
- An `EasingFunction` value

```ts
easing: {kind: 'cubicBezier', x1: 0.2, y1: 0, x2: 0, y2: 1}
```

### Channels and cancellation

`channel` names an animation so independent animations can run at once and be
targeted individually:

- Scheduling again on the **same** channel replaces that channel's animation.
- `window.cancelAnimation(channel)` cancels just that channel.
- `window.cancelAnimation()` (no argument) cancels **all** channels.

The default config uses separate channels for open/close, minimize, and the
workspace-switch visual, so e.g. switching workspaces while a window is still
finishing its open animation doesn't interrupt the open.

```ts
const OPEN = 'open';
const WORKSPACE = 'workspace-visual';

window.scheduleAnimation({channel: OPEN, /* … */});
window.scheduleAnimation({channel: WORKSPACE, /* … */}); // runs alongside OPEN
window.cancelAnimation(WORKSPACE);                        // cancels only WORKSPACE
```

### Real example: open & close

From the default window manager — note `rect` uses `add` (a decaying offset) and
`opacity` uses `multiply` (a fade that composes with the base opacity):

```ts
function scheduleOpenAnimation(window) {
  window.scheduleAnimation({
    channel: 'open',
    rect: {
      from: {x: 0, y: 200, width: 0, height: 0},
      to:   {x: 0, y: 0,   width: 0, height: 0},
      duration: 500, easing: WINDOW_OPEN_EASING, mode: 'add',
    },
    opacity: {from: 0, to: 1, duration: 500, easing: WINDOW_OPEN_EASING, mode: 'multiply'},
  });
}

function scheduleCloseAnimation(window) {
  window.setCloseAnimationDuration(500); // keep the surface alive for the fade
  window.scheduleAnimation({
    channel: 'close',
    rect: {
      from: {x: 0, y: 0, width: 0, height: 0},
      to:   {x: 0, y: 120, width: 0, height: 0},
      duration: 500, easing: WINDOW_CLOSE_EASING, mode: 'add',
    },
    opacity: {from: 1, to: 0, duration: 500, easing: WINDOW_CLOSE_EASING, mode: 'multiply'},
  });
}
```

For close animations, pair `scheduleAnimation` with
`window.setCloseAnimationDuration(ms)` so the compositor keeps the surface alive
long enough to play the animation before destroying it.

---

## Which should I use?

| Use… | When |
| --- | --- |
| **`scheduleAnimation`** | Animating the managed window's position/size/opacity — opens, closes, minimize, move, resize, workspace transitions. It's the lighter path (Rust interpolates, no per-frame TS), and `add` mode composes cleanly with live layout changes. |
| **Signal-driven `window.animation`** | Animating decoration you build in TSX — title-bar colors, hover/focus feedback, GPU `transform`/`opacity` derived from arbitrary logic. You get full flexibility at the cost of per-frame TS re-evaluation. |

They can be combined: drive the window's entrance with `scheduleAnimation` while
a focus glow on the border is driven by a signal variable.

Signal-driven animations advance on the clock of the window's output, once per
frame of that output. For your own per-frame or timed work, see
[Frame Timing & Polls](./timing.md).
