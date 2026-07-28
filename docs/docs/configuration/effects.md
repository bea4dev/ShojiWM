---
sidebar_position: 10
---

# Effects

ShojiWM can run GPU shader effects in four places, configured via
`COMPOSITOR.effect`:

| Field | Type | Applies to |
| --- | --- | --- |
| `background_effect` | `CompiledEffectHandle \| null` | Behind client-requested regions (`ext-background-effect-v1`) |
| `window` | `(window) => WindowEffectAssignment \| null` | Per toplevel window |
| `layer` | `(layer) => LayerEffectAssignment \| null` | Per layer-shell surface (bars, docks) |
| `popup` | `(popup) => PopupEffectAssignment \| null` | Per popup (menus, tooltips) |

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

### Stages

| Stage | Purpose |
| --- | --- |
| `dualKawaseBlur({radius, passes})` | Fast, wide blur |
| `shaderStage(shader, {uniforms, textures})` | Run a custom GLSL fragment shader |
| `noise({...})` | Add film-grain style noise |
| `save(name)` / `blend(input, {...})` | Save/composite intermediate results |
| `unit(effect)` | Embed a compiled reusable sub-effect |
| `renderTo(state, {...})` | Run a side pipeline into persistent state |

For `dualKawaseBlur`, `radius` defaults to `8` and controls the sampling offset;
`passes` defaults to `2` and controls the downsample/upsample depth (clamped to
`0`–`8`). Neither value automatically changes `capturePadding` or
`damagePadding`.

`shaderStage` takes a shader (a path, or a `loadShader(path)` handle) plus
`uniforms` (numbers/colors passed to the shader) and `textures` (extra source
handles bound by name).

Uniform values may be numbers or 2/3/4-component arrays, and each component may
be a signal. `tex`, `effect_texture_size_px`, and `effect_content_rect_px` are
reserved compositor bindings and cannot be used as custom uniform or texture
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
    vec2 texture_uv;       // normalized coordinates over the full working texture
    vec2 texture_size_px;  // full working-texture size in physical pixels
    vec4 content_rect_px;  // visible content: x, y, width, height in that texture
};
```

That gives you these built-ins for free inside `shader_main`:

| Name | Type | Meaning |
| --- | --- | --- |
| `effect.texture_uv` | `vec2` | Normalized coordinates over the complete texture, including capture padding |
| `effect.texture_size_px` | `vec2` | Complete working-texture size in physical pixels |
| `effect.content_rect_px` | `vec4` | Visible content rectangle as `(x, y, width, height)` inside the texture |
| `tex` | `sampler2D` | This stage's pipeline input; sample it with `texture2D(tex, effect.texture_uv)` |

All `*_px` values are physical pixels. ShojiWM also provides:

| Helper | Result |
| --- | --- |
| `effect_texture_px(effect)` | Current fragment position in the complete working texture |
| `effect_content_px(effect)` | Current fragment position relative to the visible content's top-left |
| `effect_content_uv(effect)` | Content-relative normalized coordinates; `0.0`–`1.0` over visible content and outside that range in padding |
| `effect_texture_uv_from_content_px(effect, px)` | Convert content-relative physical pixels back to texture UV for sampling |

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
    vec2 sample_px = effect_content_px(effect) + displacement_px;
    vec2 sample_uv = clamp(
        effect_texture_uv_from_content_px(effect, sample_px),
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
