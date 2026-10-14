---
sidebar_position: 8.7
---

# Drag and drop

When you drag something from one window to another (text, a file, a link),
the application usually hands ShojiWM a small picture to carry under the
pointer. `COMPOSITOR.dnd` lets your config decide what is drawn during the
drag and what happens to it after the drop.

Nothing is animated for you. Your config shows a **drag view** and drives its
position, opacity and scale with signals, so a pop-in, a "fly back" after a
refused drop or a fade is ordinary [animation](./animations.md) code. The
view's content is a tree like a window decoration, with
[paint shaders](./paint.md) and [`<ShaderEffect>`](./effects.md) available.

```tsx
import {Box, COMPOSITOR, DragIcon, Label} from 'shoji_wm';

COMPOSITOR.dnd.event.onStart((drag) => {
  const view = drag.show({
    content: () => (
      <Box direction="column" style={{alignItems: 'center', gap: 4}}>
        <DragIcon />
        <Label text={drag.action.value === 'copy' ? '+ Copy' : ''} />
      </Box>
    ),
  });
  drag.onEnd(() => view.dispose());
});
```

The [default config](./default-config.md) uses this for a frosted card with
a badge for the action and a pill naming what is dragged; a drop nobody
accepts flies back to where the drag started. Its source is
`packages/config/src/drag-and-drop.tsx`, a good starting point for your own.

## Without a listener

While no `onStart` listener is registered, ShojiWM draws the application's
picture under the pointer, as other compositors do. As soon as one listener
is registered, ShojiWM draws **only** the views your listeners show: a
listener that shows nothing hides the picture.

## The drag session

`COMPOSITOR.dnd.event.onStart(listener)` calls `listener` with a
`DragSession` when a drag starts. It returns a function that removes the
listener. `COMPOSITOR.dnd.current` is a signal holding the drag in progress,
or `null`.

| Member | Type | Meaning |
| --- | --- | --- |
| `id` | `number` | Unique per drag |
| `mimeTypes` | `readonly string[]` | The formats the data is offered in. The data itself is never exposed |
| `actions` | `{copy, move, ask}` | The actions the source allows |
| `action` | `ReadonlySignal<DragAction>` | What a drop would do now: `"copy"`, `"move"`, `"ask"`, or `"none"` |
| `target` | `ReadonlySignal<WaylandWindow \| null>` | The window under the pointer |
| `sourceWindow` | `WaylandWindow \| null` | The window the drag started from |
| `device` | `"pointer" \| "touch"` | What is dragging |
| `origin` | `{x, y}` | The pointer when the drag started (global logical coordinates) |
| `icon` | `ReadonlySignal<DragIconInfo \| null>` | The application's picture: `width`, `height` and `offsetX` / `offsetY` (its top-left corner relative to the pointer), or `null` without one |
| `ended` | `ReadonlySignal<DragEnd \| null>` | `null` until the drag ends |
| `animation` | `AnimationController` | Animations of this drag, like `window.animation` |
| `show(options)` | `DragView` | Draw a view for this drag ([below](#drag-views)) |
| `onEnd(listener)` | `() => void` | Called once with the `DragEnd`; returns an unsubscribe function |

`action` is `"none"` both over an empty spot and over a window that refuses
the data. To tell them apart, check `target`:

```ts
const refused = drag.action.value === 'none' && drag.target.value !== null;
```

`mimeTypes` tell what kind of data it is, for example:

| Formats | Usually |
| --- | --- |
| `text/uri-list` | Files from a file manager |
| `text/x-moz-url`, `_NETSCAPE_URL` | A link from a browser |
| `image/*` | An image |
| `text/plain`, `UTF8_STRING` | Text |

### How a drag ends

`DragEnd`, passed to `onEnd` and held by `ended`:

| Field | Meaning |
| --- | --- |
| `kind` | `"dropped"` (the button was released) or `"cancelled"` (the application gave up) |
| `accepted` | `true` when a target took the data. A drop over nothing or on a window that refused it is `false` |
| `target` | The window the drop landed on, or `null` |
| `position` | The pointer when the drag ended (global logical coordinates) |

## Drag views

`drag.show(options)` draws a view and returns a `DragView`. The view stays
until you call `view.dispose()`, **also after the drag ended**, so you can
animate the drop. Call `show` more than once for several views.

| Option | Type | Default | Meaning |
| --- | --- | --- | --- |
| `anchor` | `"pointer" \| "global"` | `"pointer"` | What `x` / `y` are measured from |
| `x`, `y` | `number` | `0` | With `"pointer"`: an offset from the pointer. With `"global"`: global logical coordinates |
| `opacity` | `number` | `1` | Opacity of the whole view, `0`..`1` |
| `scale` | `number` | `1` | Scale around the anchor point |
| `content` | tree or `() => tree` | the icon alone | What to draw ([below](#content)) |
| `maxLinger` | `number` (ms) | `5000` | How long the view may stay after the drag ended if `dispose()` never comes |

Every option except `content` and `maxLinger` may be a signal (or a
`computed`), and the view follows it.

`DragView` has `dispose()`, `disposed`, and `closed`, a promise that resolves
once the view is gone.

### The anchor point

The view is placed by its **anchor point**: the spot in the view where the
pointer is. If the content has a `<DragIcon/>`, that is where the pointer is
on the application's picture, so with `anchor: "pointer"` and `x = y = 0` the
picture sits under the pointer exactly where the application put it, and the
rest of the content is laid out around it. Without a `<DragIcon/>`, the
anchor point is the content's top-left corner.

`scale` scales the view around that point too, so a shrinking view shrinks
toward the pointer.

With `anchor: "pointer"`, ShojiWM moves the view along with the pointer by
itself; moving the mouse runs none of your code. Switch to `"global"` when
the view should stop following, typically once the drag has ended:

```ts
const view = drag.show({
  anchor: computed(() => (drag.ended.value ? 'global' : 'pointer')),
  x: computed(() => drag.ended.value?.position.x ?? 0),
  y: computed(() => drag.ended.value?.position.y ?? 0),
});
```

### Content

`content` is a tree of the same components as a
[window decoration](./components.md): `Box`, `Label`, `AppIcon`, `Image`,
paint shaders and `<ShaderEffect>`. Put `<DragIcon/>` where the application's
picture goes. Its children are drawn **instead** when the application gave
no picture:

```tsx
<DragIcon>
  <Label text="📄" />
</DragIcon>
```

Only the first `<DragIcon/>` gets the picture. `<AppIcon/>` shows the icon of
the window the drag started from.

When `content` is a function, it runs again whenever a signal it reads
changes. Read slow-changing state there (`drag.action`, `drag.target`,
`drag.icon`). Each run lays the content out and draws it again, so drive
animations through `x`, `y`, `opacity` and `scale` instead: those only move
the finished picture.

`<ShaderEffect>` with `backdropSource()` shows what is behind the view (the
windows, and the view's own parts below the effect). This is how the default
config blurs behind its card:

```tsx
const blur = compileEffect({
  input: backdropSource(),
  capturePadding: 24,
  invalidate: {kind: 'on-source-damage-box', damagePadding: 8},
  alpha: 'preserve',
  pipeline: [dualKawaseBlur({radius: 4, passes: 2})],
});

<Box style={{borderRadius: 10, overflow: 'hidden'}}>
  <ShaderEffect shader={blur}>
    <Box style={{background: '#1f243099', padding: 6}}>
      <DragIcon />
    </Box>
  </ShaderEffect>
</Box>
```

Some limits:

- **`opacity` below 1 also fades the effect**, so what is behind shows
  through the blur. Keep `opacity` at 1 while the view is meant to look
  solid, and only fade it in and out.
- **While `scale` is not 1**, what an effect shows is slightly off from what
  is behind it. This is barely visible during a short pop-in or shrink.
- The view cannot be clicked (the pointer is busy dragging): `onClick` and
  other handlers are ignored. `windowSource()` has nothing to show.

## Animating a drop

Animate with `drag.animation`, which works like `window.animation`. Its
`start()` returns a promise that resolves to `true` once the animation
reaches its target (and `false` if it was cut short), so disposing of the
view afterwards is one line.

This view pops in, follows the pointer, and when the drag ends either fades
in place (accepted) or flies back to where the drag started (refused):

```tsx
import {
  COMPOSITOR, DragIcon, animationVariable, computed,
} from 'shoji_wm';

const APPEAR = animationVariable('drag-appear');
const FINISH = animationVariable('drag-finish');

COMPOSITOR.dnd.event.onStart((drag) => {
  const appear = drag.animation.variable(APPEAR);
  const finish = drag.animation.variable(FINISH);
  drag.animation.start(APPEAR, {from: 0, to: 1, duration: 200});

  const at = (axis: 'x' | 'y') =>
    computed(() => {
      const end = drag.ended.value;
      if (!end) return 0; // following the pointer
      const from = end.position[axis];
      return end.accepted ? from : from + (drag.origin[axis] - from) * finish.value;
    });

  const view = drag.show({
    anchor: computed(() => (drag.ended.value ? 'global' : 'pointer')),
    x: at('x'),
    y: at('y'),
    opacity: computed(() => (drag.ended.value ? 1 - finish.value : appear.value)),
    scale: computed(() => 0.85 + 0.15 * appear.value),
    content: <DragIcon />,
  });

  drag.onEnd((end) => {
    drag.animation
      .start(FINISH, {from: 0, to: 1, duration: end.accepted ? 150 : 400})
      .then(() => view.dispose());
  });
});
```

## Changing the cursor during a drag

[`COMPOSITOR.cursor.overrideKind`](./cursor.md#overriding-the-cursor-shape)
shows a cursor shape of your choice, whatever the window under the pointer
asks for. For example, a "no drop" cursor while the window under the pointer
refuses the data:

```ts
import {COMPOSITOR, effect} from 'shoji_wm';

COMPOSITOR.dnd.event.onStart((drag) => {
  const stop = effect(() => {
    const refused = drag.action.value === 'none' && drag.target.value !== null;
    COMPOSITOR.cursor.overrideKind(refused ? 'no-drop' : null);
  });
  drag.onEnd(() => {
    stop();
    COMPOSITOR.cursor.overrideKind(null);
  });
});
```

Remember to give the cursor back with `overrideKind(null)` when the drag
ends.
