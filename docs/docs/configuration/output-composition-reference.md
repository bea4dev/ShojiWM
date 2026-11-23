---
sidebar_position: 10.61
---

# Output composition reference

Every node and helper of [output composition](./output-composition.md), all exported
from `shoji_wm`. Props marked *signal* accept a plain value or a signal; reading a
signal there re-evaluates the composition when it changes.

## `COMPOSITOR.rendering.composition`

```ts
COMPOSITOR.rendering.composition = (output: OutputInfo) => CompositionRenderable;
```

Called per output. Return a JSX tree of the nodes below (fragments and arrays nest
freely; `null`, `false` and `undefined` draw nothing). Leave it unset, or return
`<DefaultComposition />`, for the default stacking. A composition with no nodes draws
nothing at all, so return the default rather than `null`.

## Nodes

Children draw back to front: later siblings are on top.

### `<Layers>`

Layer-shell surfaces of the output.

| Prop | Type | |
| --- | --- | --- |
| `layers` | *signal* `LayerName \| LayerName[]` | `"background"`, `"bottom"`, `"top"`, `"overlay"`, listed back to front. |

Each layer's surfaces keep their own order (newest in front). Layers may appear in
several nodes or in any order, e.g. Top below the windows.

### `<Windows>`

| Prop | Type | |
| --- | --- | --- |
| `windows` | *signal* `(WaylandWindow \| string)[]` | Exactly these windows (objects or ids), in stacking order, **drawn even when hidden** (as they would look shown). Omit for the output's own stack. |
| `offsetX`, `offsetY` | *signal* `number` | Shift the windows, in logical pixels. |

Without `windows`, the node is the output's own stack: what the output shows, with
closing windows and decoration popups. That stack is the one the fullscreen fast path
and direct scanout apply to, and only at the root of the composition. With `windows`,
windows outside the composition's area are skipped.

When the topmost window of that stack is fullscreen, `"top"` layers listed after
the node are not drawn: a bar stays behind the window, as it does for the pointer.
Only `"overlay"` stays in front. An X11 window (through xwayland-satellite) whose
rectangle covers the whole output counts as fullscreen too, as in KWin and Mutter:
that is how many games make "borderless fullscreen".

### `<LayerPopups>`

Popups of layer-shell surfaces (bar menus and tooltips), all layers. No props.

### `<TextureView>`

A render texture drawn flat.

| Prop | Type | |
| --- | --- | --- |
| `texture` | `RenderTexture` | From `renderTexture`. |
| `x`, `y`, `width`, `height` | *signal* `number` | Where, in logical pixels of the composition. Omitted: the whole area. The texture stretches to fit. |
| `opacity` | *signal* `number` | 0 to 1 (default 1). |

### `<Solid>`

A solid fill.

| Prop | Type | |
| --- | --- | --- |
| `color` | *signal* `CompositionColor` | `"#rgb"`, `"#rgba"`, `"#rrggbb"`, `"#rrggbbaa"`, or `[r, g, b, a]` in 0 to 1. |
| `x`, `y`, `width`, `height` | *signal* `number` | As for `<TextureView>`. |

### `<Scene3D>`

A 3D scene of `<Plane>`s, rendered offscreen with a depth buffer and drawn as one
layer of the composition.

| Prop | Type | |
| --- | --- | --- |
| `camera` | *signal* `Camera` | `{ projection, view }` matrices; see [cameras](#cameras). |
| `x`, `y`, `width`, `height` | *signal* `number` | The scene's viewport. Omitted: the whole area. |
| `clearColor` | *signal* `CompositionColor` | Behind the planes (default transparent). |
| `antialias` | *signal* `boolean` | 4x multisampled edges (default true). |

Planes are drawn far to near; translucent planes blend over what lies behind them.
Fully transparent parts of a texture (a window texture's empty shadow margin) do not
hide what is behind them. Planes at the same depth fight; keep overlapping planes
apart in z.

### `<Plane>`

A textured rectangle, only inside `<Scene3D>`.

| Prop | Type | |
| --- | --- | --- |
| `texture` | `RenderTexture` | What the plane shows. |
| `width`, `height` | *signal* `number` | Size in world units, centred on the plane's origin. |
| `transform` | *signal* `Transform3D \| Mat4` | Placement in the world (default identity: facing the camera at the origin). |
| `opacity` | *signal* `number` | 0 to 1 (default 1). |
| `doubleSided` | *signal* `boolean` | Draw the back face too (default true). |

### `<DefaultComposition />`

The default stacking: `Layers[background, bottom]`, `Windows`,
`Layers[top, overlay]`, `LayerPopups`.

## `renderTexture(options)`

```ts
renderTexture({
  content: CompositionRenderable,   // the nodes to render
  key?: string,                     // keeps the GPU texture across re-evaluations
  width?: number, height?: number,  // logical size (default: the output's)
  scale?: number,                   // pixel density (default: the output's scale)
  clearColor?: CompositionColor,    // default transparent
}): RenderTexture
```

The content is laid out like the output: layers and windows appear at their usual
places relative to the output's top-left corner, clipped to the texture's size.
A texture renders once per frame, however often it is used, and incrementally.
Without `key`, textures are told apart by call order.

## 3D helpers

### `transform3d(matrix?)`

Starts a `Transform3D`, built like CSS `transform`: each call applies in the local
space of the calls before it. Angles are in degrees.

| Method | |
| --- | --- |
| `translate(x, y = 0, z = 0)` | Move. |
| `rotateX(deg)`, `rotateY(deg)`, `rotateZ(deg)` | Turn about an axis. |
| `scale(x, y = x, z = 1)` | Scale. |
| `multiply(other)` | Apply another `Transform3D` or `Mat4`. |
| `.matrix` | The column-major `Mat4`. |

Transforms are immutable, so a shared base can branch:

```ts
const cube = transform3d().translate(0, 0, -half).rotateY(angle);
const front = cube.translate(0, 0, half);
const right = cube.rotateY(90).translate(0, 0, half);
```

### Cameras

| Helper | |
| --- | --- |
| `screenCamera(output \| { width, height }, { fov = 45, distance = 0 })` | Maps z = 0 onto the output 1:1. `distance` pulls the camera back (zooms out). |
| `perspective(fovY, aspect, near, far)` | OpenGL-style projection matrix; `fovY` in degrees. |
| `lookAt(eye, target, up = [0, 1, 0])` | View matrix of a camera at `eye` looking at `target`. |
| `multiplyMat4(a, b)` | `a × b`, column-major. |
| `outputLogicalSize(output)` | `{ width, height }` of an output in logical pixels. |

A hand-built camera:

```ts
const { width, height } = outputLogicalSize(output);
const camera = {
  projection: perspective(50, width / height, 1, 10_000),
  view: lookAt([0, 300, 1600], [0, 0, 0]),
};
```

### Picking

| Helper | |
| --- | --- |
| `projectPoint(camera, viewport, [x, y, z])` | Where a world point lands, `{ x, y, depth }` in logical pixels from the viewport's top-left corner (`depth`: smaller is nearer), or `null` behind the camera. |
| `pickPlane(camera, viewport, planes, x, y)` | Index of the nearest plane under `(x, y)`, or `null`. `planes` take the same `width`, `height` and `transform` as their `<Plane>`s. |

`viewport` is `{ width, height }` of the `<Scene3D>`. With an
[input grab](./keybindings-and-pointer.md#input-grab), this makes a 3D layout
clickable:

```ts
const { width, height } = outputLogicalSize(output);
const hit = pickPlane(screenCamera(output), { width, height }, planes,
  event.position.x - output.position.x, event.position.y - output.position.y);
```

## Types

| Type | |
| --- | --- |
| `CompositionRenderable` | What a composition function returns. |
| `RenderTexture` | Result of `renderTexture`. |
| `Camera` | `{ projection: Mat4; view: Mat4 }`. |
| `Mat4` | 16 numbers, column-major (WebGL/glMatrix layout). |
| `LayerName` | `"background" \| "bottom" \| "top" \| "overlay"`. |
| `CompositionColor` | Hex string (`"#rgb"` to `"#rrggbbaa"`) or `[r, g, b, a]` in 0 to 1 (straight alpha). |
