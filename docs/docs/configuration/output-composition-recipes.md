---
sidebar_position: 10.62
---

# Output composition recipes

Complete effects built from [output composition](./output-composition.md). Each one
is a function that returns `null` while the effect is idle; fall back to the default
stacking then, which also keeps the fullscreen fast path:

```tsx
COMPOSITOR.rendering.composition = (output) =>
  crossfade(output) ?? <DefaultComposition />;
```

Return the default rather than `null` from the composition function itself: a
composition with no nodes draws nothing.

All names below are exported from `shoji_wm`.

## A cube between any two views

`examples/output-composition/cube-transition.tsx` turns a cube between any two pieces
of composition, e.g. two workspaces:

```tsx
import { COMPOSITOR, DefaultComposition, Layers, Windows } from "shoji_wm";
import { createCubeTransition } from "./cube-transition";

const cube = createCubeTransition({ duration: 700 });
COMPOSITOR.rendering.composition = (output) =>
  cube.compose(output) ?? <DefaultComposition />;

// When switching (switch the windows themselves without animation):
cube.start(outputName, {
  from: <><Layers layers={["background", "bottom"]} /><Windows windows={fromIds} /></>,
  to: <><Layers layers={["background", "bottom"]} /><Windows windows={toIds} /></>,
  direction: 1,
});
```

## Dimming everything but the bars

No texture needed: put a translucent fill between the windows and the bars.

```tsx
const [dimmed, setDimmed] = signal(false);

COMPOSITOR.rendering.composition = () => (
  <>
    <Layers layers={["background", "bottom"]} />
    <Windows />
    {dimmed() && <Solid color="#00000099" />}
    <Layers layers={["top", "overlay"]} />
    <LayerPopups />
  </>
);
```

`<Windows />` stays at the root, so the fullscreen fast path still works while the
fill is off.

## Crossfade

Fade from a snapshot of one view to another with two texture views:

```tsx
const [fade, setFade] = signal<number | null>(null); // 0 → 1

function crossfade(output: OutputInfo) {
  const t = fade();
  if (t === null) return null;
  const from = renderTexture({ key: "fade-from", content: <Windows windows={fromIds} /> });
  const to = renderTexture({ key: "fade-to", content: <Windows windows={toIds} /> });
  return (
    <>
      <Layers layers={["background", "bottom"]} />
      <TextureView texture={from} opacity={1 - t} />
      <TextureView texture={to} opacity={t} />
      <Layers layers={["top", "overlay"]} />
      <LayerPopups />
    </>
  );
}
```

Drive `fade` from a `createPoll` on the output and set it back to `null` at the end.

## Zoom

Zoom into a point of the screen by scaling a texture of the desktop about it. The 3D
scene clips the plane to the output, so any zoom level works:

```tsx
function zoomed(output: OutputInfo) {
  const k = zoom(); // 1 = no zoom
  if (k <= 1) return null;
  const { width, height } = outputLogicalSize(output);
  const desktop = renderTexture({ key: "zoom", content: <DefaultComposition />, scale: 2 });
  // The focus point in world coordinates (origin at the centre, +Y up).
  const cx = focusX() - width / 2;
  const cy = height / 2 - focusY();
  return (
    <Scene3D camera={screenCamera(output)}>
      <Plane texture={desktop} width={width} height={height}
        transform={transform3d().translate(cx * (1 - k), cy * (1 - k)).scale(k)} />
    </Scene3D>
  );
}
```

`scale: 2` renders the texture at twice the pixel density, so text stays sharp up to
2x. Input is not zoomed: it still goes to the windows where they really are.

## Per-window textures

A texture can frame a single window: size it to the window and shift the window to
the texture's corner with `offsetX`/`offsetY`. Use `window.rect`, the whole
decorated window (`window.position` is the client area without the title bar), and
leave a margin for shadows.

```tsx
const MARGIN = 48;

function windowTexture(output: OutputInfo, window: WaylandWindow) {
  const { x, y, width, height } = window.rect;
  return renderTexture({
    key: `window-${window.id}`,
    width: width + MARGIN * 2,
    height: height + MARGIN * 2,
    content: (
      <Windows
        windows={[window]}
        offsetX={output.position.x - x + MARGIN}
        offsetY={output.position.y - y + MARGIN}
      />
    ),
  });
}
```

Each such texture redraws only when its window changes, so a dozen of them cost little
while they stand still. Picked by id, the window shows even when it is minimized or on
another workspace.

### A 3D window switcher

With one texture per window, a Windows Vista "Flip 3D" style switcher is a row of
planes. The default config ships one: `packages/config/src/flip-3d.tsx`
(`Super` + `Tab`, see [Flip 3D](./default-config.md#flip-3d)). Its parts:

- **Layout.** Each window is a `<Plane>` of its texture in one `<Scene3D>` with
  `screenCamera(output)`. A window's *real pose* (translation to its centre, no
  turn, scale 1) puts the plane exactly where the window is on screen; its *slot
  pose* puts it in the stack. Opening interpolates real → slot, closing slot → real,
  so the desktop morphs into the stack and back. Interpolate the numbers (position,
  angle, scale, opacity), not the matrices.
- **Input.** An [input grab](./keybindings-and-pointer.md#input-grab) takes the
  keyboard, clicks, wheel and swipes while the switcher is open, and
  [`pickPlane`](./output-composition-reference.md#picking) turns a click into a window.
  Releasing `Super` (held since `Super` + `Tab`) arrives as a key release.
- **Switching.** Ask the window manager to activate the chosen window *without* its
  own animations (another workspace, a restore from minimized) and let the planes fly
  to the windows' new positions.
- **Hand-off.** Return to `<DefaultComposition />` once every plane sits on its
  window, which with an ease-out curve happens a little before the end.

None of this depends on the stack's shape. The default config keeps it in
`window-switcher.tsx` and puts only the layout and its input in separate files:
`flip-3d.tsx` for the stack, and `window-grid.tsx` for a flat overview grid
([Window grid](./default-config.md#window-grid)).

**Blur inside per-window textures.** A window alone in its texture has nothing below
it, so a backdrop blur on it (a glass titlebar, say) blurs empty space while still
costing a blur per window. Switch those effects off while the switcher is open by
reading a signal where the decoration picks them, and give the effects a last stage
whose strength is a signal, so they fade back in after the hand-off:

```glsl
// backdrop-fade.frag: 0 = no effect (transparent), 1 = the effect as it is
uniform float strength;
vec4 shader_main(EffectContext effect) {
    vec4 color = texture2D(tex, effect.texture_uv);
    color.a = 1.0;
    return color * strength;
}
```

```tsx
const glass = compileEffect({
  input: backdropSource(),
  alpha: "preserve",
  pipeline: [
    dualKawaseBlur({ radius: 4, passes: 2 }),
    shaderStage(loadShader("./src/effect/backdrop-fade.frag"), {
      uniforms: { strength: backdropStrength },
    }),
  ],
});
// in the decoration:
blurSuspended() ? <Box style={titlebar}>…</Box> : <ShaderEffect shader={glass} style={titlebar}>…</ShaderEffect>
```
