---
sidebar_position: 3
---

# Outputs (Displays)

`COMPOSITOR.output` controls monitor layout — resolution, refresh rate, scale,
position, mirroring, and enabling/disabling outputs. You both **read** the
current state and **register a factory** that produces the desired layout.

## Configuring outputs

`COMPOSITOR.output.configure(factory)` registers a function the compositor calls
**every time the set of connected outputs changes** (hotplug, dock, undock).
The factory returns a map of `output name → config entry`.

```ts
import {COMPOSITOR, type DisplayConfigDraft} from 'shoji_wm';

COMPOSITOR.output.configure((context) => {
  const display: DisplayConfigDraft = {};

  display['DP-1'] = {
    mode: 'extend',
    resolution: {width: 2560, height: 1440, refreshRate: 144},
    position: 'auto',
    scale: 1.5,
  };
  display['eDP-1'] = {mode: 'extend', resolution: 'best', scale: 1.8};

  // Turn off the laptop panel while docked
  const docked = context.connected.some((o) => o.name === 'HDMI-A-1');
  if (docked) {
    display['eDP-1'] = {mode: 'disabled'};
  }

  return display;
});
```

The output name (`"DP-1"`, `"eDP-1"`, `"HDMI-A-1"`, …) is the DRM connector name.
List the connected names by reading `context.connected` or `COMPOSITOR.output.list`.

### Config entry: `mode`

Each entry has a `mode` that selects one of three shapes:

| `mode` | Meaning | Extra fields |
| --- | --- | --- |
| `"extend"` *(default)* | Use the output as part of the desktop | `resolution`, `position`, `scale`, `transform` |
| `"disabled"` | Turn the output off | — |
| `"mirror"` | Mirror another output | `source` (name of the output to mirror) |

```ts
display['HDMI-A-1'] = {mode: 'mirror', source: 'eDP-1'};
display['eDP-2'] = {mode: 'disabled'};
```

`mode` may be omitted for an extend entry (it is the default).

### `resolution`

Selects the DRM mode (size + refresh rate).

| Value | Meaning |
| --- | --- |
| `"best"` | Highest resolution + refresh rate the output advertises |
| `{width, height}` | Pick a mode of that size (highest matching refresh rate) |
| `{width, height, refreshRate}` | Pick that exact mode |

```ts
display['DP-1'] = {resolution: 'best'};
display['DP-2'] = {resolution: {width: 1920, height: 1080}};
display['DP-3'] = {resolution: {width: 2560, height: 1440, refreshRate: 165}};
```

Inspect what a monitor supports with `COMPOSITOR.output.availableModes(name)`.

### `position`

Where the output sits in the global coordinate space.

| Value | Meaning |
| --- | --- |
| `"auto"` *(default)* | Compositor places it automatically (left-to-right) |
| `{x, y}` | Explicit top-left corner in logical pixels |

```ts
display['DP-1'] = {position: {x: 0, y: 0}};
display['DP-2'] = {position: {x: 2560, y: 0}}; // to the right of DP-1
```

### `scale`

Fractional scale factor (HiDPI). `1.0` is native; `2.0` doubles UI size; the
default config uses values like `1.5`–`1.8`.

```ts
display['eDP-1'] = {resolution: 'best', scale: 1.8};
```

### `transform`

Rotation / flip of the output, following the standard `wl_output.transform`
enum. Use it for vertically mounted (portrait) monitors or inverted panels.

| Value | Meaning |
| --- | --- |
| `"normal"` *(default)* | No rotation |
| `"rotate-90"` / `"rotate-180"` / `"rotate-270"` | Rotate the picture by 90° / 180° / 270° |
| `"flipped"` | Mirror horizontally |
| `"flipped-90"` / `"flipped-180"` / `"flipped-270"` | Mirror, then rotate |

```ts
// A monitor physically rotated into portrait orientation
display['DP-1'] = {
  resolution: 'best',
  position: 'auto',
  transform: 'rotate-90',
};
```

Notes:

- `resolution` always refers to the **physical (unrotated) mode** — a portrait
  1080×1920 setup still selects `{width: 1920, height: 1080}` (or `'best'`).
- The config draft is declarative: omitting `transform` (or removing it later)
  resets the output to `"normal"`.
- Everything downstream sees the **transformed orientation**: `OutputInfo.
  resolution` reports the rotated size (so `resolution / scale` is always the
  logical size), `usableArea`, tiling, screenshots (`grim`) and screen capture
  (OBS via the portal) all follow the rotation automatically.

## Reading output state

The controller is also a read-only view, useful inside event handlers and the
composition function.

| Member | Returns |
| --- | --- |
| `list` | `string[]` — names of connected, enabled outputs |
| `outputs` | `OutputInfo[]` — snapshot of every output |
| `current` | `Record<string, OutputInfo>` — snapshots keyed by name |
| `get(name)` | `OutputInfo \| undefined` |
| `find(predicate)` | first matching `OutputInfo` |
| `availableModes(name)` | `OutputMode[]` reported by the driver |
| `configure(factory)` | register a layout factory (above) |
| `reconfigure()` | re-run all registered factories now |

`OutputInfo` includes `name`, `enabled`, `resolution` (`{width, height,
refreshRate}`), `position` (`{x, y}`), `scale`, `transform`, `availableModes`,
and identification fields (`make`, `model`, `serial`, `connector`).

On a transformed output, `resolution` is reported in the **rotated
orientation** (width/height swapped for 90°/270°), while `availableModes` stay
physical. This keeps `resolution / scale` equal to the logical size in every
case.

```ts
const hz = COMPOSITOR.output.get('DP-1')?.resolution?.refreshRate;

// Logical size of an output (resolution divided by its scale)
const out = COMPOSITOR.output.get('DP-1');
if (out?.resolution) {
  const widthLogical = out.resolution.width / out.scale;
  const heightLogical = out.resolution.height / out.scale;
}
```

:::tip
`COMPOSITOR.output.configure` is for hardware layout. To place windows so they
don't overlap bars/docks, use `COMPOSITOR.layer.usableArea(name)` instead, which
subtracts exclusive-zone layer surfaces.
:::
