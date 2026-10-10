---
sidebar_position: 10
---

# Effects

ShojiWM can run GPU shader effects in these places, configured via
`COMPOSITOR.effect`:

| Field | Type | Applies to |
| --- | --- | --- |
| `background_effect` | `MaybeSignal<CompiledEffectHandle \| null>` | Behind client-requested regions (`ext-background-effect-v1`) |
| `window` | `(window) => WindowEffectAssignment \| null` | Per toplevel window |
| `layer` | `(layer) => LayerEffectAssignment \| null` | Per layer-shell surface (bars, docks) |
| `popup` | `(popup) => PopupEffectAssignment \| null` | Per popup (menus, tooltips) |
| [`overlay`](./output-overlays.md) | `(output, options) => Promise<OverlayHandle>` | Temporary or persistent fullscreen output effects |

You can also apply an effect to a region inside the composition with
[`<ShaderEffect/>`](./components.md#shadereffect).

## Background effect

`background_effect` is the effect the compositor renders **behind the regions a
client requests through the `ext-background-effect-v1` Wayland protocol** (a
`blur_region` declared on its surface). It is **not** a global full-screen
backdrop: a window or layer-shell surface opts in via the protocol, and the
compositor applies this effect behind that region only — for example a
translucent app or panel that asks the compositor to blur whatever is behind it.
Set `null` to disable.

```ts
import {COMPOSITOR, compileEffect, backdropSource, dualKawaseBlur} from 'shoji_wm';

COMPOSITOR.effect.background_effect = compileEffect({
  input: backdropSource(),
  capturePadding: 24,
  invalidate: {kind: 'on-source-damage-box', damagePadding: 8},
  pipeline: [dualKawaseBlur({radius: 4, passes: 2})],
});
```

The value may be a signal, and the shader uniforms inside the effect may be
signals too; the compositor picks up a change on the next frame. This turns the
effect off or animates it, for example:

```ts
const blur = compileEffect({
  input: backdropSource(),
  alpha: 'preserve',
  pipeline: [
    dualKawaseBlur({radius: 4, passes: 2}),
    shaderStage(loadShader('./src/effect/backdrop-fade.frag'), {
      uniforms: {strength}, // a signal from 0 to 1
    }),
  ],
});
COMPOSITOR.effect.background_effect = computed(() => (paused() ? null : blur));
```

## Per-window / layer / popup effects

Each factory is called per surface and returns an assignment, or `null`/`{}` for
no effect. Layer and popup assignments use `behind` to render the effect beneath
the surface (the default config blurs everything behind bars and menus):

```ts
const LAYER_BLUR = compileLayerEffect({
  input: backdropSource(),
  capturePadding: 24,
  invalidate: {kind: 'on-source-damage-box', damagePadding: 8},
  alpha: 'preserve',
  pipeline: [dualKawaseBlur({radius: 4, passes: 2})],
});

COMPOSITOR.effect.layer = (layer) => {
  if (layer.namespace() === 'no_blur') return {};
  return {behind: LAYER_BLUR};
};

COMPOSITOR.effect.popup = (popup) => {
  if (popup.parentKind === 'window') return {};
  return {behind: POPUP_BLUR};
};
```

### Narrowing a layer effect to a region

A layer's `behind` backdrop covers the whole layer surface by default: the
backdrop is captured over it, the pipeline runs over it, and any damage anywhere
under it re-runs the effect. Shell toolkits often map surfaces much larger than
what they draw — a fixed-size surface around an animated pill, or one full-screen
surface per monitor with the bar cut out by an input mask — which makes that
expensive. Set `region` to cover only the part that draws:

```ts
const BAR_BLUR = compileLayerEffect({
  input: backdropSource(),
  region: 'input', // the input region, e.g. QuickShell's `mask`
  outsets: 16,     // what draws past the region: antialiasing, merging shapes
  alpha: 'preserve',
  pipeline: [dualKawaseBlur({radius: 4, passes: 2})],
});
```

| `region` | Covers |
|---|---|
| `'surface'` (default) | The whole surface. |
| `'input'` | The bounding box of the input region (`wl_surface.set_input_region`). A surface without one takes input everywhere, so this is then the whole surface. |
| `'blur-region'` | The bounding box of the blur region the client asked for through ext-background-effect (QuickShell's `BackgroundEffect.blurRegion`). Nothing is drawn while the client asks for none. |

The capture, the pipeline, `layerSource()` and damage-based invalidation all
follow the region, plus `outsets`. Both regions are part of the same commit as
the buffer, so the effect never lags a frame behind an animated shape. Anything
the surface draws outside the region gets no backdrop. Only a backdrop `behind`
accepts a region other than `'surface'`.

## Blurring specific windows or layers

A common setup is to blur behind only some applications — a translucent
terminal, say — and only some bars. Both are a matter of reading the surface's
identity inside the function that decides what it gets.

### Telling surfaces apart

| Surface | Identity | Where to read it |
| --- | --- | --- |
| Window | `window.appId()` — the Wayland `app_id`, or the X11 `WM_CLASS` class for Xwayland apps (e.g. `"kitty"`, `"org.gnome.Nautilus"`, `"steam"`) | `COMPOSITOR.window.composition` |
| Window | `window.title()` | `COMPOSITOR.window.composition` |
| Layer surface | `layer.namespace()` — chosen by the client (e.g. waybar's `"waybar"`; QuickShell's `WlrLayershell.namespace`) | `COMPOSITOR.effect.layer` |

Both are signals, so the decision is re-made when the value changes (a browser
that sets its `app_id` after mapping still ends up with the right effect). To
find the value for an app you use, log it once and look in
`~/shoji_wm/logs/latest.log`:

```ts
COMPOSITOR.effect.layer = (layer) => {
  console.info('layer namespace:', layer.namespace());
  return {};
};
```

### Windows: wrap the decoration in `<ShaderEffect/>`

A window's background blur is part of its decoration. Put the title bar and the
`<ClientWindow/>` inside a [`<ShaderEffect/>`](./components.md#shadereffect)
for the apps you want, and a plain `<Box/>` for the rest. The effect fills the
whole decorated area and follows the rounded corners of the
[`<WindowBorder/>`](./components.md#windowborder) around it.

```tsx
import {
  COMPOSITOR, compileEffect, backdropSource, dualKawaseBlur,
  ManagedWindow, WindowBorder, ShaderEffect, Box, Label, ClientWindow,
} from 'shoji_wm';

// Compile once, outside the composition function.
const FROSTED = compileEffect({
  input: backdropSource(),
  capturePadding: 24,
  invalidate: {kind: 'on-source-damage-box', damagePadding: 8},
  alpha: 'preserve',
  pipeline: [dualKawaseBlur({radius: 4, passes: 2})],
});

const BLURRED_APPS = ['kitty', 'ghostty', 'org.gnome.Nautilus'];

COMPOSITOR.window.composition = (window) => {
  const content = [
    <Box direction="row" style={{height: 28, paddingX: 8, alignItems: 'center'}}>
      <Label text={window.title} style={{flexGrow: 1, fontSize: 13}} />
    </Box>,
    <ClientWindow />,
  ];
  const blurred = BLURRED_APPS.includes(window.appId() ?? '');

  return (
    <ManagedWindow rect={window.position} zIndex={1}>
      <WindowBorder style={{borderRadius: 10, border: {px: 2, color: '#4f5666'}}}>
        {blurred ? (
          <ShaderEffect shader={FROSTED} direction="column">{content}</ShaderEffect>
        ) : (
          <Box direction="column">{content}</Box>
        )}
      </WindowBorder>
    </ManagedWindow>
  );
};
```

The default config does exactly this for kitty and ghostty (search for
`TERMINALS` in `packages/config/src/index.tsx`), with the liquid-glass shader
below instead of a plain blur.

:::note The application has to be translucent
The blur is drawn **behind** the client, so it only shows through where the
client's own pixels are transparent. Turn on the app's own transparency, e.g.
`background_opacity 0.8` in kitty or `background-opacity = 0.8` in ghostty. An
opaque window looks unchanged; only its title bar (if it is translucent) shows
the effect.
:::

### Stronger liquid glass

The default config's liquid-glass stage bends the backdrop only slightly. Its
uniforms control the look:

| Uniform | Effect |
| --- | --- |
| `distortion_depth` | How far in from the edge the bend reaches, as a fraction of the window's shorter side. Larger values make a wider lens rim. |
| `distortion_strength` | How far the backdrop is pulled at the rim. This is the main "how much glass" knob. |
| `chromatic_shift_px` | Color fringing at the rim, in pixels. `0` turns it off. |
| `glass_tint` | Brightness multiplier for the backdrop. Below `1` darkens it, which keeps text readable. |
| `glass_radius_px` | Corner radius of the lens; `-1` follows the window's rounded corners. |

```ts
shaderStage(loadShader('./src/effect/liquid-glass.frag'), {
  uniforms: {
    glass_radius_px: -1.0,
    distortion_depth: 0.35,    // default config: 0.2
    distortion_strength: 0.4,  // default config: 0.15
    chromatic_shift_px: 4.0,
    glass_tint: 0.85,
  },
}),
```

### Layers: choose by namespace

Bars, docks and launchers are layer surfaces, so they go through
`COMPOSITOR.effect.layer` instead. Return `{behind: ...}` for the namespaces
to blur and `{}` for everything else:

```ts
const PANEL_BLUR = compileLayerEffect({
  input: backdropSource(),
  capturePadding: 24,
  invalidate: {kind: 'on-source-damage-box', damagePadding: 8},
  alpha: 'preserve',
  pipeline: [dualKawaseBlur({radius: 4, passes: 2})],
});

const BLURRED_LAYERS = ['waybar', 'rofi'];

COMPOSITOR.effect.layer = (layer) =>
  BLURRED_LAYERS.includes(layer.namespace() ?? '') ? {behind: PANEL_BLUR} : {};
```

The default config does the opposite — it blurs every layer and lets a client
opt out with the `no_blur` namespace — and clips the blur to the layer's own
opaque pixels with `layerSource()` so a bar with rounded or floating parts is
not blurred in its transparent gaps (`LAYER_BLUR_MASK` in the default config).

## Building an effect

An effect is **a source input + a pipeline of stages**. Compile it with the
function matching where it will be used:

| Compiler | Produces | For |
| --- | --- | --- |
| `compileEffect(opts)` | `CompiledEffectHandle` | background, `<ShaderEffect/>` |
| `compileWindowEffect(opts)` | `WindowEffectHandle` | `COMPOSITOR.effect.window` |
| `compileLayerEffect(opts)` | `LayerEffectHandle` | `COMPOSITOR.effect.layer` |
| `compilePopupEffect(opts)` | `PopupEffectHandle` | `COMPOSITOR.effect.popup` |

Options:

| Option | Type | Meaning |
| --- | --- | --- |
| `input` | source handle | What the pipeline reads from (e.g. `backdropSource()`) |
| `capturePadding` | `MaybeSignal<number>` | Extra logical pixels captured around the visible content and processed by every stage (default `0`) |
| `pipeline` | stage array | Stages applied in order |
| `invalidate` | policy | When to re-render (default: source damage with zero padding) |
| `alpha` | `"opaque" \| "preserve"` | Final output-alpha policy (default `"opaque"`) |
| `outsets` | `EffectOutsets` | (window/layer/popup effects) render beyond the surface bounds |

### Sources

| Source | Reads |
| --- | --- |
| `backdropSource()` | The composited scene behind the target |
| `xrayBackdropSource()` | The backdrop as if the current surface were absent |
| `windowSource()` | The window's own surface |
| `layerSource()` | The layer surface's own content |
| `popupSource()` | The popup's own content |
| `imageSource(path)` | A static image file |
| `shaderInput(shader, opts)` | A shader-generated input texture |
| `get(name)` | An intermediate previously stored by `save(name)` |
| `stateSource(state)` | The latest value of a persistent state texture |

`windowSource`, `layerSource`, and `popupSource` accept
`{include: 'full' | 'root-surface'}`. The default is `'full'`.

On the TTY backend, a full window source includes the root surface,
subsurfaces, and server-side decorations. A full-source replacement applies
to that entire image; subsurfaces are not drawn a second time above it.
Popups remain independently composed. A root-surface source continues to
exclude subsurfaces.

### Subsurfaces

Subsurfaces (embedded video, previews, some browser content) may reach outside
the window, where a full window source cannot cover them. Two window-only
slots handle them separately, over their own bounds: `replaceSubsurfaces`
replaces them, and `behindSubsurfaces` is drawn behind them (plus its
`outsets`), e.g. for a drop shadow.

```ts
COMPOSITOR.effect.window = () => ({
  replace: DISSOLVE,
  replaceSubsurfaces: DISSOLVE,
  behind: SHADOW,
  behindSubsurfaces: SHADOW,
});
```

- `windowSource()` in these slots reads the subsurfaces; `behindSubsurfaces`
  sees them before `replaceSubsurfaces`. While either slot is set, the window
  sources of all other slots leave subsurfaces out (root surface plus
  server-side decorations only).
- `behindSubsurfaces` sits directly behind its subsurfaces, so for a
  subsurface inside the window it is drawn over the window's content.
- Subsurfaces above and below the root surface are processed as two groups so
  their stacking order is kept. Without a `replace` effect, the lower group
  stays between the client and its decorations; with one, it is drawn beneath
  the replacement.
- The texture covers the subsurfaces, not the window. To line a mask up with
  the window, compute it with `effect_frame_uv(effect)` (see
  [the shader contract](#the-shader_main-contract)); the same shader then
  produces one continuous mask across the window and subsurface slots.
- A failing effect falls back to drawing the subsurfaces as they are.
- TTY backend only. Close animations render from a frozen snapshot of the
  client area, which does not use these slots.

### Stages

| Stage | Purpose |
| --- | --- |
| `dualKawaseBlur({radius, passes})` | Fast, wide blur |
| `shaderStage(shader, {uniforms, textures})` | Run a custom GLSL fragment shader |
| `noise({...})` | Add film-grain style noise |
| `save(name)` / `blend(input, {...})` | Save/composite intermediate results |
| `unit(effect)` | Embed a compiled reusable sub-effect |
| `renderTo(state, {...})` | Run a side pipeline into persistent state |
| `renderToIfDirty(state, {dependsOn, ...})` | Like `renderTo`, but only re-run when a declared source changed |

For `dualKawaseBlur`, `radius` defaults to `8` and controls the sampling offset;
`passes` defaults to `2` and controls the downsample/upsample depth (clamped to
`0`–`8`). Neither value automatically changes `capturePadding` or
`damagePadding`.

`shaderStage` takes a shader (a path, or a `loadShader(path)` handle) plus
`uniforms` (numbers/colors passed to the shader) and `textures` (extra source
handles bound by name).

Uniform values may be numbers or 2/3/4-component arrays, and each component may
be a signal. `tex`, `effect_texture_size_phy_px`, `effect_content_rect_phy_px`, and
`effect_frame_rect_phy_px` (and their former `_px` spellings) are reserved compositor bindings and cannot be used as custom uniform or texture
names.

```ts
import {compileEffect, backdropSource, dualKawaseBlur, shaderStage, loadShader} from 'shoji_wm';

const liquidGlass = compileEffect({
  input: backdropSource(),
  capturePadding: 24,
  invalidate: {kind: 'on-source-damage-box', damagePadding: 8},
  pipeline: [
    dualKawaseBlur({radius: 4, passes: 2}),
    shaderStage(loadShader('./src/liquid-glass.frag'), {
      uniforms: {
        glass_radius_px: 10.0,
        distortion_strength: 0.15,
        chromatic_shift_px: 3.0,
      },
    }),
  ],
});
```

### Persistent state textures

`save()` and `get()` are frame-local intermediate storage. For simulations,
trails, accumulated masks, and other effects that must retain GPU data between
frames, declare a `stateTexture()` and update it with `renderTo()`.

```ts
const velocity = stateTexture('velocity', {
  scale: 0.5,
  format: 'rg16f',
  resize: 'clear',
});
const dye = stateTexture('dye', {
  scale: 0.5,
  format: 'rgba16f',
  resize: 'stretch',
});

const fluid = compileLayerEffect({
  input: layerSource(),
  invalidate: {kind: 'manual', dirtyWhen: fluidActive},
  pipeline: [
    renderTo(velocity, {
      input: stateSource(velocity),
      pipeline: [
        shaderStage(loadShader('./velocity.frag'), {
          textures: {dye: stateSource(dye)},
          uniforms: {dt: frameDelta},
        }),
      ],
    }),
    renderTo(dye, {
      input: stateSource(dye),
      pipeline: [
        shaderStage(loadShader('./dye.frag'), {
          textures: {velocity: stateSource(velocity)},
        }),
      ],
    }),
    shaderStage(loadShader('./fluid-compose.frag'), {
      textures: {dye: stateSource(dye)},
    }),
  ],
});
```

Each window/layer/popup effect instance owns separate state even when it shares
the same handle. `renderTo()` does not replace the outer pipeline's current
texture. `stateSource()` reads the previous frame before the first write, then
the newest value after each write. Reading and writing the same state is safe:
the compositor uses ping-pong textures internally.

`scale` is relative to the effect processing size. Formats are `rgba8`,
`rg16f`, and `rgba16f`; `rg16f` currently uses an RGBA16F allocation internally
and exposes the meaningful state in the R/G channels. On resize, `clear`
restarts from transparent black while `stretch` preserves and scales the latest
state. State survives compatible hot reloads and is released with or evicted
from the owning effect-instance cache.

#### Skipping a side pipeline that did not change: `renderToIfDirty()`

`renderTo()` runs every time the outer pipeline runs, which is what temporal
effects need. Some side pipelines are different: they are expensive but depend
only on the subject itself — a distance field or mask built from a layer's
silhouette, for example — while the outer pipeline has to re-run whenever the
backdrop underneath changes. `renderToIfDirty()` re-runs such a side pipeline
only when one of the sources listed in `dependsOn` changed since the state was
last written, and skips it entirely otherwise.

```ts
const field = stateTexture('silhouette-field', {format: 'rgba16f'});

const glass = compileLayerEffect({
  input: backdropSource(),
  invalidate: {kind: 'on-source-damage-box', damagePadding: 64},
  pipeline: [
    dualKawaseBlur({radius: 2, passes: 2}),
    renderToIfDirty(field, {
      dependsOn: [layerSource()],
      input: layerSource(),
      pipeline: [shaderStage(loadShader('./src/field.frag'))],
    }),
    shaderStage(loadShader('./src/glass.frag'), {
      textures: {field: stateSource(field)},
    }),
  ],
});
```

- `dependsOn` takes `windowSource()`, `layerSource()` and `popupSource()`. A
  source counts as changed when the content captured for it changed (a new
  commit, a different size or position inside the capture).
- **Declare every source the side pipeline really reads.** The compositor
  cannot look inside GLSL, so an undeclared dependency leaves a stale result.
  Note that a `shaderStage` always receives the previous stage as `tex`; if the
  shader samples it, whatever produced it is a dependency too. Start the
  compositor with `SHOJI_RENDER_TO_IF_DIRTY_ALWAYS=1` to make every
  `renderToIfDirty()` behave like `renderTo()` and check whether a glitch is a
  missing dependency.
- **Read the result with `stateSource(state)`.** Names `save()`d inside the
  side pipeline do not exist on skipped runs, so `get()`ting one from outside
  the side pipeline is a configuration error. Using them inside is fine.
- The state keeps the side pipeline's final texture as-is, without the copy
  `renderTo()` performs, so it is in the same coordinate system as the
  textures `save()` / `get()` expose inside the side pipeline.
- It also re-runs by itself when the state is reallocated (first use, resize,
  format change) and when the side pipeline's own uniforms change.

### Invalidation policy

`invalidate` controls when the effect re-renders, trading freshness for cost:

- `{kind: 'on-source-damage-box', damagePadding: N}` — re-render only the
  region that changed, extending both damage detection and the redraw region by
  `N` logical pixels. The usual choice.
- `{kind: 'always'}` — re-render every frame (expensive; for animated shaders).
- `{kind: 'manual', dirtyWhen, base?}` — re-render while the `MaybeSignal<boolean>`
  `dirtyWhen` is true. While false, reuse the cached result unless the optional
  automatic `base` policy also invalidates it.

For example, a manually controlled effect that still responds to nearby source
damage can use:

```ts
invalidate: {
  kind: 'manual',
  dirtyWhen: effectParametersChanged,
  base: {kind: 'on-source-damage-box', damagePadding: 24},
}
```

`damagePadding` does not add pixels to a shader's input. Use `capturePadding`
when blur, distortion, or another sampling operation needs source pixels beyond
the visible content boundary.

### Capture padding

With `capturePadding: N`, ShojiWM captures `N` extra logical pixels around the
effect's visible content. The padded texture passes through the entire pipeline,
including custom shaders, blur stages, saved intermediates, and blends. ShojiWM
crops it back to the visible content only once, after the last stage.

This avoids edge clamping in effects that sample neighboring pixels. The value
is expressed in logical pixels; ShojiWM applies the output scale when allocating
the physical texture. By contrast, the `EffectContext` pixel fields passed to
GLSL are physical pixels. A value of `0` keeps the exact visible bounds.

ShojiWM cannot infer a custom shader's sampling reach. Set `capturePadding` large
enough for the farthest sample and normally set `damagePadding` to cover the
same affected source area. Texture edge clamping is only a safety behavior; it
repeats the cropped edge pixel and cannot recover scene pixels that were never
captured. At a physical output edge, unavailable padding is naturally clipped.

### Alpha

With the default `alpha: 'opaque'`, the final pass forces alpha to `1.0`. This is
appropriate for ordinary backdrop blur, whose captured edge alpha is not useful
content. Set `alpha: 'preserve'` when the pipeline intentionally produces
transparency (for example, a blur clipped to a layer's own alpha mask). In that
mode the shader pipeline is responsible for meaningful alpha throughout the
working texture.

---

## Writing custom shaders

When the built-in stages aren't enough, you can write your own **fragment
shader** and run it with `shaderStage`. A fragment shader is a small program the
GPU runs **once for every pixel** of the region; its job is to compute that
pixel's final color. ShojiWM shaders are written in **GLSL ES 1.00** (the same
dialect as WebGL 1 / OpenGL ES 2.0) and live in `.frag` files next to your
config.

:::tip[New to shaders?]
If you have never written a shader before, the per-pixel mindset takes a moment
to click. These are excellent, beginner-friendly introductions — read one first,
then come back:

- [The Book of Shaders](https://thebookofshaders.com/) — the gentlest start
- [Shadertoy](https://www.shadertoy.com/) — experiment live in the browser
- [Khronos GLSL ES quick reference](https://www.khronos.org/files/opengles_shading_language.pdf) — the built-in functions

The GLSL you write for ShojiWM is the same language; only the entry point and a
few provided variables (below) differ.
:::

### The `shader_main` contract

You don't write a full GLSL program — you write **one function**:

:::warning[Shader ABI]
The current ABI requires `shader_main(EffectContext)`. Shaders using the former
`shader_main(vec2 uv, vec2 rect_size)` signature must be migrated.
:::

```glsl
vec4 shader_main(EffectContext effect) {
    // compute and return this pixel's color
    return vec4(1.0, 0.0, 0.0, 1.0); // opaque red
}
```

ShojiWM defines `EffectContext` and constructs it before calling your function.
The relevant contract is equivalent to:

```glsl
struct EffectContext {
    vec2 texture_uv;           // normalized coordinates over the full working texture
    vec2 texture_size_phy_px;  // full working-texture size in physical pixels
    vec4 content_rect_phy_px;  // visible content: x, y, width, height in that texture
    vec4 frame_rect_phy_px;    // the window the effect belongs to, in that texture
    vec4 frame_radius_phy_px;  // its corner radii: top-left, top-right, bottom-right, bottom-left
    vec4 frame_border_phy_px;  // its border widths: top, right, bottom, left
    vec4 clip_rect_phy_px;     // the clip ShojiWM applies to the effect, in that texture
    vec4 clip_radius_phy_px;
    bool has_clip;
    float scale;               // physical pixels per logical pixel
};
```

That gives you these built-ins for free inside `shader_main`:

| Name | Type | Meaning |
| --- | --- | --- |
| `effect.texture_uv` | `vec2` | Normalized coordinates over the complete texture, including capture padding |
| `effect.texture_size_phy_px` | `vec2` | Complete working-texture size in physical pixels |
| `effect.content_rect_phy_px` | `vec4` | Visible content rectangle as `(x, y, width, height)` inside the texture |
| `effect.frame_rect_phy_px` | `vec4` | For window effects, the window's own rectangle (without `outsets`) inside the texture, even when the texture covers something else (`replaceSubsurfaces`). Otherwise the content rectangle |
| `effect.frame_radius_phy_px` | `vec4` | Corner radii of the frame (top-left, top-right, bottom-right, bottom-left) |
| `effect.frame_border_phy_px` | `vec4` | Border widths of the frame (top, right, bottom, left) |
| `effect.clip_rect_phy_px` | `vec4` | The rounded clip ShojiWM applies to the effect, `(x, y, width, height)` inside the texture; only valid when `has_clip` |
| `effect.clip_radius_phy_px` | `vec4` | Corner radii of that clip |
| `effect.has_clip` | `bool` | Whether the effect is clipped (e.g. by a window's rounded corners) |
| `effect.scale` | `float` | Physical pixels per logical pixel; multiply logical lengths by it to look the same at every scale |
| `tex` | `sampler2D` | This stage's pipeline input; sample it with `texture2D(tex, effect.texture_uv)` |

What the frame is depends on where the effect runs:

| Placement | Frame (rect, radius, border) | Clip |
| --- | --- | --- |
| `<ShaderEffect/>` | The component itself (`borderRadius`, borders) | Rounded ancestors, e.g. the window's corners |
| Window slots | The window (the `<WindowBorder>`, or the root); for `windowSource({include: 'root-surface'})` the client area inside the border | — |
| Layer, popup, background | The covered area; radius and border are 0 (the client draws its own shape) | — |

All `*_phy_px` values are physical pixels (logical pixels times the output
scale). ShojiWM also provides:

| Helper | Result |
| --- | --- |
| `effect_texture_phy_px(effect)` | Current fragment position in the complete working texture |
| `effect_content_phy_px(effect)` | Current fragment position relative to the visible content's top-left |
| `effect_content_uv(effect)` | Content-relative normalized coordinates; `0.0`–`1.0` over visible content and outside that range in padding |
| `effect_texture_uv_from_content_phy_px(effect, px)` | Convert content-relative physical pixels back to texture UV for sampling |
| `effect_frame_phy_px(effect)` | Current fragment position relative to the frame's top-left |
| `effect_frame_uv(effect)` | Frame-relative normalized coordinates; `0.0`–`1.0` over the window, outside that range beyond it |
| `effect_frame_sdf(effect)` | Signed distance to the frame's rounded rect (negative inside) |
| `effect_frame_coverage(effect)` | Coverage of the frame's rounded rect, antialiased over one physical pixel |
| `effect_frame_inner_sdf(effect)` | Signed distance to the inner edge of the frame's border |
| `effect_clip_sdf(effect)` / `effect_clip_coverage(effect)` | The same for the clip (always inside without one) |
| `shoji_rrect_sdf(p, rect, radius)` / `shoji_coverage(sdf)` | General rounded-rect distance and coverage, as in [paint shaders](./paint.md#helpers) |

For example, an edge highlight that follows the window's corners:

```glsl
vec4 shader_main(EffectContext effect) {
    vec4 color = texture2D(tex, effect.texture_uv);
    float d = effect.has_clip ? effect_clip_sdf(effect) : effect_frame_sdf(effect);
    float rim = 1.0 - smoothstep(0.0, 1.5 * effect.scale, abs(d + 0.75 * effect.scale));
    return vec4(color.rgb + rim * 0.25, color.a);
}
```

:::note[Former names]
Before the `_phy_px` suffix these were called `texture_size_px`,
`content_rect_px`, `frame_rect_px`, `effect_texture_px`, `effect_content_px`,
`effect_texture_uv_from_content_px` and `effect_frame_px`. The old names still
work and carry the same values; new shaders should use the `_phy_px` names.
:::

Use texture UV for sampling and content coordinates for geometry tied to the
visible rectangle. The content rectangle can start at a non-zero offset because
capture padding precedes it in the working texture.

Return the pixel color as a `vec4(r, g, b, a)` with components in `0.0`–`1.0`.

For a distortion expressed in physical pixels, convert the displaced content
position back to texture UV. Do not clamp content UV to `0.0`–`1.0` first;
doing so throws away the pixels supplied by `capturePadding`.

```glsl
uniform vec2 displacement_px;

vec4 shader_main(EffectContext effect) {
    vec2 sample_px = effect_content_phy_px(effect) + displacement_px;
    vec2 sample_uv = clamp(
        effect_texture_uv_from_content_phy_px(effect, sample_px),
        vec2(0.0),
        vec2(1.0)
    );
    return texture2D(tex, sample_uv);
}
```

### A first shader: a solid color

The simplest possible shader ignores everything and returns one color:

```glsl
// shaders/white.frag
vec4 shader_main(EffectContext effect) {
    return vec4(1.0, 1.0, 1.0, 1.0); // opaque white
}
```

Wire it into an effect with `loadShader` + `shaderStage`, then use that effect
anywhere an effect is accepted (here, a [`<ShaderEffect/>`](./components.md#shadereffect)):

```tsx
import {compileEffect, backdropSource, shaderStage, loadShader} from 'shoji_wm';

const white = compileEffect({
  input: backdropSource(),
  pipeline: [shaderStage(loadShader('./shaders/white.frag'))],
});

<ShaderEffect shader={white} style={{width: 100, height: 40}} />
```

### Reading the source texture

Usually you want to transform what's *behind* the region rather than paint a flat
color. Sample the source with `texture2D(tex, effect.texture_uv)`. This shader desaturates the
backdrop to grayscale:

```glsl
// shaders/grayscale.frag
vec4 shader_main(EffectContext effect) {
    vec4 source = texture2D(tex, effect.texture_uv);
    float gray = dot(source.rgb, vec3(0.299, 0.587, 0.114));
    return vec4(vec3(gray), source.a);
}
```

`texture2D(tex, effect.texture_uv)` returns the source pixel under the current fragment as a
`vec4` (RGBA). From there it's ordinary math.

### Parameters: uniforms

To make a shader configurable, declare `uniform` variables and pass their values
from `shaderStage`. A uniform is the same value for every pixel in a draw.

```glsl
// shaders/tint.frag
uniform vec3 tint;       // an RGB color
uniform float strength;  // 0..1

vec4 shader_main(EffectContext effect) {
    vec4 source = texture2D(tex, effect.texture_uv);
    vec3 tinted = mix(source.rgb, tint, strength);
    return vec4(tinted, source.a);
}
```

```ts
shaderStage(loadShader('./shaders/tint.frag'), {
  uniforms: {
    tint: [0.84, 0.73, 0.49], // vec3
    strength: 0.3,            // float
  },
});
```

The TypeScript value type maps to the GLSL uniform type by length:

| `uniforms` value | GLSL type |
| --- | --- |
| `number` | `float` |
| `[number, number]` | `vec2` |
| `[number, number, number]` | `vec3` |
| `[number, number, number, number]` | `vec4` |

GLSL arrays use the explicit `uniformArray.float/vec2/vec3/vec4` helpers. The
array length must match the GLSL declaration and is treated as shader-binding
structure. Changing values uses the native uniform fast path; changing the
length rebuilds the binding.

```glsl
uniform float weights[3];
uniform vec2 control_points[2];
```

```ts
import {signal, uniformArray} from 'shoji_wm';

const phase = signal(0);

shaderStage(loadShader('./shaders/weighted.frag'), {
  uniforms: {
    weights: uniformArray.float([1, phase, 0.25]),
    control_points: uniformArray.vec2([
      [0, 0],
      [phase, 1],
    ]),
  },
});
```

The array itself, each scalar element, and every vector component may be a
signal. Arrays must be non-empty. Keep their length fixed during animation;
replace the effect or composition structure when a different length is needed.

### Animating a shader

Every uniform value (each component) may be a **signal**, so you animate a shader
by feeding it a changing value — there is no built-in `time`. Drive a `phase`
uniform from an [animation variable](./animations.md) or any signal, and set the
effect's [`invalidate`](#invalidation-policy) to `{kind: 'always'}` so it re-renders each
frame:

Here `window` comes from the composition function's argument (a per-window effect
factory, `COMPOSITOR.effect.window = (window) => {…}`, gives you a `window` the
same way):

```tsx
import {animationVariable} from 'shoji_wm';

const pulse = animationVariable('pulse');

COMPOSITOR.window.composition = (window: WaylandWindow) => {
  // start the loop somewhere, e.g. on open:
  // window.animation.start(pulse, {duration: 1000, repeat: 'ping-pong'});

  const glow = compileEffect({
    input: backdropSource(),
    invalidate: {kind: 'always'},
    pipeline: [
      shaderStage(loadShader('./shaders/glow.frag'), {
        uniforms: {
          phase: window.animation.variable(pulse), // a signal → animates
          intensity: 0.8,
        },
      }),
    ],
  });

  return (
    <ManagedWindow rect={window.position}>
      <ShaderEffect shader={glow}>
        <ClientWindow />
      </ShaderEffect>
    </ManagedWindow>
  );
};
```

```glsl
// shaders/glow.frag
uniform float phase;     // 0..1, animated from TS
uniform float intensity;

vec4 shader_main(EffectContext effect) {
    vec4 source = texture2D(tex, effect.texture_uv);
    float k = 1.0 + intensity * phase;
    return vec4(source.rgb * k, source.a);
}
```

### Extra textures

Beyond the implicit `tex`, you can bind more textures: declare each as a
`uniform sampler2D`, and pass a source handle by the same name in `textures`.
This is how a layer's own content is used as a mask:

```glsl
// shaders/layer-mask.frag
uniform sampler2D layer_mask;
uniform float opacity_threshold;
uniform float mask_feather;

vec4 shader_main(EffectContext effect) {
    vec4 blurred = texture2D(tex, effect.texture_uv);      // implicit source
    float a = texture2D(layer_mask, effect.texture_uv).a;  // extra texture
    float mask = smoothstep(opacity_threshold - mask_feather,
                            opacity_threshold + mask_feather, a);
    return blurred * mask;
}
```

```ts
shaderStage(loadShader('./shaders/layer-mask.frag'), {
  textures: {layer_mask: layerSource()},
  uniforms: {opacity_threshold: 0.25, mask_feather: 0.04},
});
```

Extra sources are aligned to the same padded working texture. Pixels outside an
extra source's visible content are transparent, so all samplers use the same
`effect.texture_uv` coordinate system.

### GLSL ES 1.00 reminders

A few things to keep in mind in this dialect (it's older than desktop GLSL):

- Sample textures with **`texture2D(...)`**, not `texture(...)`.
- `precision highp float;` is already declared — don't redeclare it.
- Don't use `in` / `out` / `layout`; just write and `return` from `shader_main`.
- `for` loops need a **constant** loop bound (no dynamic length).
- Handy built-ins: `mix`, `clamp`, `smoothstep`, `step`, `length`, `dot`,
  `fract`, `floor`, `abs`, `min`, `max`, `sin`, `cos`, `pow`.

### Iterating

Shaders are loaded from disk by path, so editing a `.frag` and triggering a
config reload re-compiles it — no full restart needed. Start from a shader that
samples `tex` and returns it unchanged, then change one line at a time.
