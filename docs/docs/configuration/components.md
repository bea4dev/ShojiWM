---
sidebar_position: 8
---

# SSD Components

These are the building blocks you assemble inside
[`COMPOSITOR.window.composition`](./window-composition.md) to draw window
decorations. They are imported from `shoji_wm`:

```tsx
import {Box, Label, Button, AppIcon, Image, Popup, ShaderEffect, WindowBorder} from 'shoji_wm';
```

`<ManagedWindow/>` and `<ClientWindow/>` are documented on the
[Window composition](./window-composition.md) page.

## Common props

Every component accepts these (from `ComponentProps`):

| Prop | Type | Meaning |
| --- | --- | --- |
| `children` | nodes | Child components |
| `style` | `SSDStyle` | Visual styling — see [Style reference](#style-reference) |
| `id` | `string` | Stable node id for targeted invalidation |
| `onHoverChange` | `(hovered: boolean) => void` | Pointer enter/leave |
| `onActiveChange` | `(active: boolean) => void` | Press/release |
| `paint` | `PaintShaderHandle` | Replaces the built-in background/border — see [Paint shaders](./paint.md) |
| `overlay` | `PaintShaderHandle` | Painted above the children — see [Paint shaders](./paint.md) |

All `style` values (and most props) accept either a plain value or a signal, so
they update reactively.

---

## `<Box/>`

A flexbox-like container that arranges children horizontally or vertically.

| Prop | Type | Meaning |
| --- | --- | --- |
| `direction` | `"row" \| "column" \| "horizontal" \| "vertical"` | Layout axis (default `"row"`) |
| `split` | `Direction` | Split direction for two-panel layouts |
| `style` | `SSDStyle` | Styling |

```tsx
<Box direction="row" style={{gap: 8, padding: 4, alignItems: 'center'}}>
  <AppIcon icon={window.icon} style={{width: 16, height: 16}} />
  <Label text={window.title} style={{flexGrow: 1}} />
</Box>
```

## `<Label/>`

Renders a text string.

| Prop | Type | Meaning |
| --- | --- | --- |
| `text` | `string` (or signal) | The text to display |
| `style` | `SSDStyle` | Font and color via `fontSize`, `fontWeight`, `fontFamily`, `color`, `textAlign`, `lineHeight` |

```tsx
<Label
  text={window.title}
  style={{color: '#f5f7fa', fontSize: 13, fontWeight: 600, fontFamily: ['Noto Sans CJK JP', 'Noto Color Emoji']}}
/>
```

## `<Button/>`

A pressable region that triggers an action on click.

| Prop | Type | Meaning |
| --- | --- | --- |
| `onClick` | `() => void` or `WindowActionDescriptor` | Action on click |
| `onHoverChange` | `(hovered: boolean) => void` | Track hover for visual feedback |
| `style` | `SSDStyle` | Styling |

`onClick` accepts either a callback, or a descriptor from `windowAction(...)`
for the built-in window operations: `"close"`, `"maximize"`, `"unmaximize"`,
`"minimize"`, `"fullscreen"`, `"unfullscreen"`.

```tsx
import {Button, windowAction} from 'shoji_wm';

// Built-in action
<Button onClick={windowAction('close')} style={{width: 12, height: 12}} />

// Custom handler + hover feedback
const [hover, setHover] = useState(false);
<Button
  onHoverChange={setHover}
  onClick={() => window.minimize()}
  style={{width: 16, height: 16, borderRadius: 8, background: hover((h) => h ? '#FFFFFF40' : '#FFFFFF20')}}
/>
```

## `<AppIcon/>`

Renders a window's application icon.

| Prop | Type | Meaning |
| --- | --- | --- |
| `icon` | `WindowIcon \| undefined` (or signal) | Pass `window.icon` for reactive updates |
| `style` | `SSDStyle` | Sizing |

```tsx
<AppIcon icon={window.icon} style={{width: 16, height: 16}} />
```

## `<Image/>`

Displays an image from a file path (resolved relative to the config package
root) or a reactive source.

| Prop | Type | Meaning |
| --- | --- | --- |
| `src` | `string` (or signal) | Image path |
| `fit` | `"contain" \| "cover" \| "fill"` | How the image fills its box |
| `style` | `SSDStyle` | Sizing/positioning |

```tsx
<Image src="./assets/x.svg" style={{width: 16, height: 16, pointerEvents: 'none'}} />
```

## ShaderEffect

`<ShaderEffect/>` is a container that applies a compiled GPU effect to the region
its children occupy. See [Effects](./effects.md) for building the `shader`.

| Prop | Type | Meaning |
| --- | --- | --- |
| `shader` | `CompiledEffectHandle` | The compiled effect to render |
| `direction` | `Direction` | Layout axis for children (like `<Box/>`) |
| `style` | `SSDStyle` | Styling |

```tsx
<ShaderEffect shader={frostedGlass} direction="row" style={{height: 28, paddingX: 8, alignItems: 'center'}}>
  <Label text={window.title} />
</ShaderEffect>
```

## WindowBorder

`<WindowBorder/>` is a chrome container placed around `<ClientWindow/>` that draws
the border and provides interactive resize hit areas.

| Prop | Type | Meaning |
| --- | --- | --- |
| `style` | `SSDStyle` | Border via `border`, `borderRadius`, `background`, etc. |
| `interaction` | `WindowBorderInteraction` | Resize hit areas |

`interaction.resizeHitArea` is either a single number, or
`{edgePx?, cornerPx?}`. The resize grab covers the visible border plus
`edgePx` outside the frame, so it never covers client content near the edge.
`cornerPx` is how far the diagonal corner grab reaches along each edge, measured
from the outer corner of that band. Without `resizeHitArea`, only the border
itself resizes.

```tsx
<WindowBorder
  style={{border: {px: 2, color: borderColor}, borderRadius: 10}}
  interaction={{resizeHitArea: {edgePx: 8, cornerPx: 14}}}
>
  <ClientWindow />
</WindowBorder>
```

## `<Popup/>`

Content shown next to its parent but drawn **outside the window**: a tooltip
for a title bar button, a menu with buttons. It is written inside the
component it belongs to (its parent is its *anchor*), so it shares that
component's signals. The model follows the browser's
[popover API](https://developer.mozilla.org/en-US/docs/Web/API/Popover_API).

### A tooltip

```tsx
import {Box, Button, Label, Popup, windowAction} from 'shoji_wm';

<Button onClick={windowAction('maximize')} style={{width: 16, height: 16}}>
  <Popup trigger="hover" openDelay={500} placement="bottom" offset={6}>
    <Box style={{background: '#1e1e2ef0', borderRadius: 6, paddingX: 10, paddingY: 4}}>
      <Label text="Maximize" style={{fontSize: 12, color: '#f5f7fa'}} />
    </Box>
  </Popup>
</Button>
```

`trigger="hover"` opens it once the pointer has rested `openDelay` ms on the
button and closes it `closeDelay` ms after the pointer left.

### A menu with buttons

```tsx
<Button onClick={toggleMaximize}>
  <Popup trigger="hover" mode="auto" openDelay={500} closeDelay={200}>
    <Box direction="row" style={{gap: 6, padding: 6}}>
      <Button onClick={() => snap(window, 'left')}>…</Button>
      <Button onClick={() => snap(window, 'right')}>…</Button>
    </Box>
  </Popup>
</Button>
```

With `mode="auto"` the popup takes pointer input, so its buttons work. The
pointer can move from the anchor into the popup: inside the popup the anchor
still counts as hovered, and `closeDelay` covers the gap between them. A press
outside, Escape, or opening another `"auto"` popup closes it.

### A menu that opens on click, with your own state

```tsx
const [open, setOpen] = useState(false);

<Button onClick={() => setOpen(!open())}>
  <AppIcon icon={window.icon} />
  <Popup mode="auto" open={open} onOpenChange={(next, reason) => setOpen(next)}>
    <Button onClick={() => { window.close(); setOpen(false); }}>…</Button>
  </Popup>
</Button>
```

The same with less code is `<Popup trigger="click" mode="auto">`.

### Props

| Prop | Type | Meaning |
| --- | --- | --- |
| `open` | `boolean` (or signal) | Whether it is shown (default `true`); not used with `trigger` |
| `trigger` | `"hover" \| "click"` | Let the popup open and close itself (see below) |
| `openDelay` | `number` | `trigger="hover"`: ms on the anchor before it opens (default `500`) |
| `closeDelay` | `number` | `trigger="hover"`: ms after the pointer left before it closes (default `200`) |
| `mode` | `"hint" \| "auto" \| "manual"` | How it takes part in input (default `"hint"`, see below) |
| `onOpenChange` | `(open, reason) => void` | `"auto"`: the compositor asks it to close |
| `closeOnEscape` | `boolean` | `"auto"`: Escape closes it (default `true`) |
| `closeOnOutsidePress` | `boolean` | `"auto"`: a press outside it and its anchor closes it (default `true`) |
| `onInterestChange` | `(interested) => void` | The pointer entered / left the anchor or, unless `"hint"`, the popup |
| `onAnchorPress` | `() => void` | The anchor was pressed |
| `placement` | `"top" \| "bottom" \| "left" \| "right"` | The side of the anchor it goes to (default `"bottom"`) |
| `align` | `"start" \| "center" \| "end"` | Alignment along that side (default `"center"`) |
| `offset` | `number` | Distance from the anchor in logical pixels (default `0`) |
| `collision` | `"flip" \| "none"` | `"flip"` (default) moves it to the opposite side when that has more room on the output and slides it along the side to stay on the output |
| `layer` | `"top" \| "window"` | `"top"` (default): above every window. `"window"`: right above its own window, so windows stacked higher cover it |
| `style` | `SSDStyle` | Styling of the popup box itself |

### Modes

| `mode` | Pointer input | Closed by the compositor |
| --- | --- | --- |
| `"hint"` (default) | Falls through to whatever is below | Never |
| `"auto"` | Taken, also above other windows' clients | On a press outside, Escape, its window hidden, another `"auto"` popup opening |
| `"manual"` | Taken | Never |

The compositor never changes `open` itself: it calls `onOpenChange(false,
reason)` and your config decides. An `"auto"` popup without `onOpenChange` or
`trigger` gets no close requests (and leaves Escape to the window). `reason` is one of:

| `reason` | When |
| --- | --- |
| `"outside-press"` | A press outside the popup (and the popups nested in it) and outside its anchor |
| `"escape"` | Escape, for the most recently opened `"auto"` popup. While one is open, Escape does not reach the focused window; set `closeOnEscape={false}` to keep Escape for the window |
| `"anchor-gone"` | Its window was hidden (minimized, moved to another workspace) |
| `"other-popup"` | Another `"auto"` popup opened that is not nested in this one |

### Triggers

`trigger` keeps the open state for you, like `interestfor` / `popovertarget`
in browsers:

- `"hover"`: opens after the pointer rested `openDelay` ms on the anchor and
  closes `closeDelay` ms after it left both the anchor and (unless `"hint"`) the
  popup. Coming back within `closeDelay` keeps it open.
- `"click"`: each press of the anchor toggles it.

A `trigger` popup also closes on `onOpenChange` requests; your own
`onOpenChange` is still called. Set `trigger` once and do not switch it on and
off.

### Hover inside a popup

Outside popups, hover goes to the innermost node with `onHoverChange` only.
Inside an `"auto"` / `"manual"` popup it covers the whole ancestor chain, like
the DOM's `:hover`: the buttons in the popup, the popup, its anchor and the
anchor's ancestors all count as hovered. So `open={hover}` driven by the
anchor's `onHoverChange` keeps a menu open while the pointer is on it.

### How a popup differs from an ordinary child

- **Position**: it is sized by its content (its children are laid out as a
  column) and takes no space in the parent's layout.
- **Not clipped**: the rounded corners of the window and `overflow: 'hidden'`
  ancestors do not cut it.
- **Stacking**: drawn in its own pass, above every window (`layer="top"`) and
  below layer-shell bars and overlays. A popup nested in a popup is drawn with
  its outer popup.
- **Painting**: it is an ordinary decoration subtree: `style`, `boxShadow`,
  [paint shaders](./paint.md), labels, images and signal-driven `opacity` all
  work. A `<ShaderEffect/>` inside a popup draws its children but not its
  effect.
- **Window animations**: while its window is drawn with an animated transform
  (open / close, minimize, scaling), the popup is neither drawn nor hit.
  Dragging a window moves its popups with it.
- `open` only toggles visibility: a closed popup keeps its layout, so opening it
  is cheap.

### Rust

```rust
Button::new().child(
    Popup::new()
        .mode(PopupMode::Auto)
        .trigger(PopupTrigger::hover(500.0, 200.0))
        .on_open_change(|open, reason| eprintln!("open: {open} ({reason:?})"))
        .child(Label::new("Maximize")),
)
```

`Popup::new().open(signal)`, `.close_on_escape(false)`,
`.on_interest_change(...)` and `.on_anchor_press(...)` match the props above.

---

## Style reference

The `style` prop is an `SSDStyle`. Every value may be a signal. Lengths are in
logical pixels unless noted.

### Sizing

`width`, `height` (number or string like `"100%"`), `minWidth`, `minHeight`,
`maxWidth`, `maxHeight`, `flexGrow`, `flexShrink`.

### Spacing

`gap`, `padding`, `paddingX`, `paddingY`, `paddingTop/Right/Bottom/Left`,
`margin`, `marginX`, `marginY`, `marginTop/Right/Bottom/Left`.

### Layout & position

`alignItems` (`"start" | "center" | "end" | "stretch"`), `justifyContent`
(`"start" | "center" | "end" | "space-between"`), `position`
(`"relative" | "absolute"`), `inset`, `top`, `right`, `bottom`, `left`,
`zIndex`, `overflow` (`"visible" | "hidden"`), `pointerEvents`
(`"auto" | "none"`), `transform` (`{translateX, translateY, scale, scaleX, scaleY}`).

### Appearance

`background`, `color`, `opacity`, `visible`, `cursor`, `borderRadius`.

Borders: `border`, `borderTop`, `borderRight`, `borderBottom`, `borderLeft` —
each a `{px, color}` value — plus `borderFit` (`"normal" | "fit-children"`).
A side value overrides `border` for that side, both in width (the content box
moves accordingly) and color. Any non-zero width covers at least one physical
pixel.

Shadows: `boxShadow` — one `{x, y, blur, spread, color, inset}` value or an
array of them, CSS semantics. See [Paint shaders](./paint.md#shadows-boxshadow).

Colors are `#RGB`, `#RGBA`, `#RRGGBB` or `#RRGGBBAA`.

### Text (for `<Label/>`)

`fontSize`, `fontWeight` (`"normal" | "medium" | "semibold" | "bold"` or a
number), `fontFamily` (string or string array of fallbacks), `textAlign`
(`"start" | "center" | "end"`), `lineHeight`.

```tsx
const style: SSDStyle = {
  height: 28,
  paddingX: 8,
  gap: 8,
  alignItems: 'center',
  background: window.isFocused((f) => (f ? '#1f2430cc' : '#2a2f3acc')),
  borderRadius: 8,
};
```
