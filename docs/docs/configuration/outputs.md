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
| `"extend"` *(default)* | Use the output as part of the desktop | `resolution`, `position`, `scale`, `transform`, `subpixel` |
| `"disabled"` | Turn the output off | — |
| `"mirror"` | Mirror another output | `source` (name of the output to mirror), `subpixel` |

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

### `subpixel`

Physical arrangement of the panel's subpixels, advertised to clients in
`wl_output.geometry`. Clients such as foot and Firefox use it to decide how to
antialias text; most panels report `"unknown"` to the kernel, so this is often
the only way a client can learn the real layout.

| Value | Meaning |
| --- | --- |
| `"unknown"` | Layout not known (what most connectors report) |
| `"none"` | No subpixel structure, e.g. a projector |
| `"horizontal-rgb"` / `"horizontal-bgr"` | Subpixels side by side, in that order |
| `"vertical-rgb"` / `"vertical-bgr"` | Subpixels stacked, in that order |

```ts
// A laptop panel the kernel reports as "unknown", known to be RGB stripe
display['eDP-1'] = {
  resolution: 'best',
  position: 'auto',
  subpixel: 'horizontal-rgb',
};
```

Notes:

- Give the **physical** layout of the panel. The compositor sends it alongside
  `transform` in the same event, and clients combine the two themselves, so do
  not pre-rotate it for a rotated output.
- Omitting `subpixel` (or removing it later) restores whatever the kernel
  reported for the connector, which `OutputInfo.detectedSubpixel` always shows.
- Clients are told at once: every bound `wl_output` receives a fresh `geometry`
  event, so a config reload takes effect without a reconnect.

### HDR (`hdr`)

Drives the output as HDR10: a PQ / BT.2020 signal with HDR metadata. It only
takes effect when the display's EDID advertises SMPTE ST 2084 (PQ); otherwise
the output stays SDR and the log says why. Experimental, tty only.

`hdr: true` turns it on with the defaults. To change a setting, give an object
instead, which turns HDR on as well:

| Option | Meaning |
| --- | --- |
| `hdr.enabled` | `false` keeps the settings but drives the output as SDR. Default `true` |
| `hdr.sdrLuminance` | Brightness of SDR white (ordinary windows): cd/m² (10–1000), or `"backlight"` to follow the brightness setting. Default: follow the backlight on laptops, 203 elsewhere |
| `hdr.sdrGamut` | Colors: `"native"` (default) as in SDR mode, or `"srgb"` for exact colors |
| `hdr.maxLuminance` | The display's real peak in cd/m², when its EDID omits it (otherwise 1000 is assumed). Range 50–10000 |
| `hdr.minLuminance` | The display's real black level in cd/m². Range 0–10 |

```ts
display['eDP-1'] = {
  resolution: 'best',
  position: 'auto',
  hdr: true,
};

display['DP-1'] = {
  hdr: {
    // An external monitor has no backlight to follow: fix SDR white instead.
    sdrLuminance: 250,
  },
};
```

How it looks:

- **SDR content** (almost every window) keeps its look, with white shown at
  `hdr.sdrLuminance`.
- **Brightness keys keep working on laptops.** An HDR signal states absolute
  luminance, so the panel ignores its backlight while in HDR. ShojiWM applies
  the brightness setting itself instead: it moves SDR white, reaching the
  panel's suggested SDR maximum (from its EDID) at full brightness. The
  backlight value is left untouched and takes over again in SDR mode. The
  level used is the driver's `actual_brightness`, which can differ from the
  requested `brightness` (amdgpu's OLED panels apply a curve).
- **SDR colors match SDR mode.** In SDR mode a wide-gamut panel shows sRGB
  values on its own, wider primaries, which makes them more saturated than
  sRGB. HDR follows suit by default (`sdrGamut: "native"`, using the
  primaries in the panel's EDID); `"srgb"` shows exact colors instead, which on
  such a panel look duller. The choice applies to all content alike:
  color-managed clients such as Chrome hand over even their SDR interface in
  the HDR format, and an exception for them would leave them duller than every
  other window. With `"native"`, HDR video gets the same vivid look the panel
  gives video in SDR mode.
- Without a backlight, SDR white defaults to 203 cd/m², the broadcast
  reference, which can look dim on a bright monitor; raise it to taste.
- **HDR content** from color-managed clients (Chrome and Chromium, mpv, games
  through Vulkan) is shown above SDR white, up to the display's peak, and
  tone-mapped where it goes beyond. Clients learn the available headroom
  (peak ÷ SDR white) from the color-management protocol, so the higher
  `hdr.sdrLuminance` is, the less room is left for highlights.
- Screenshots and screen recordings stay SDR.

Notes:

- Whether the display supports HDR is in `OutputInfo.hdrSupported`.
- On HDMI, the link has to carry 10 bits per color at the chosen mode;
  `OutputInfo.hdmi` and `availableModes[].clockKhz` let a config check that.
- `SHOJI_HDR_OUTPUTS=eDP-1` (or `all`) turns HDR on without a config, and
  `SHOJI_SDR_NITS` sets the default for `hdr.sdrLuminance` on outputs without a
  backlight.

### Color profile (`icc`)

Applies the monitor's ICC profile, for example one made with DisplayCAL or
ArgyllCMS. Colors are converted from sRGB to what the profile says the monitor
does, so a calibrated monitor shows them accurately. Tty only.

```ts
display['DP-1'] = {
  icc: '~/.local/share/icc/DP-1.icc',
};
```

How it works:

- Windows are taken as sRGB with a 2.2 gamma (what an uncalibrated monitor
  shows) and converted with the relative colorimetric intent and black point
  compensation. On a wide-gamut monitor this makes sRGB content look like sRGB
  instead of oversaturated.
- The profile's calibration curves (`vcgt`) are applied too. Do not load them
  again with another tool such as `dispwin`.
- Content from color-managed clients is converted to sRGB first and then
  through the profile like everything else.

Notes:

- The profile applies while the output runs SDR; an output in HDR ignores it.
- It is read when the config is evaluated. After replacing the file, reload the
  config. A profile that cannot be read is logged and ignored.
- The output goes through one extra conversion pass, as in HDR, so a fullscreen
  window is not scanned out directly on it. The cursor stays a hardware cursor.
- Screenshots and screen recordings are taken before the conversion, so they
  stay plain sRGB.

## Switching panels off (DPMS)

`mode: 'disabled'` takes an output out of the desktop, so its windows move
elsewhere. To blank a screen while you are away, switch its **power** instead:
the output stays in the layout, windows stay where they are, and only the panel
goes dark. This needs a TTY session; nested sessions have no panel and ignore
it.

### From an idle daemon

ShojiWM implements `wlr-output-power-management`, so
[`wlopm`](https://git.sr.ht/~leon_plickat/wlopm) works, for example with
swayidle:

```sh
swayidle -w \
  timeout 300 'swaylock -f' \
  timeout 600 'wlopm --off "*"' resume 'wlopm --on "*"'
```

### From the config

```ts
// Every panel off until the next key press, click or pointer motion.
COMPOSITOR.key.bind('screen-off', 'Super+Shift+O', () =>
  COMPOSITOR.output.setPower('off', {wakeOnInput: true}),
);

COMPOSITOR.output.setPower('off', {output: 'HDMI-A-1'}); // one output
COMPOSITOR.output.setPower('toggle'); // off if any targeted panel is on, else on
COMPOSITOR.output.setPower('on');
```

`setPower` works from anywhere in the config: key bindings, timers, and IPC
handlers (`createIpcServer` from `shoji_wm/ipc`), so an external script can also
switch panels through an IPC method of your own. In a Rust config it is
`COMPOSITOR.output.set_power(OutputPower::Off, OutputPowerOptions { wake_on_input: true, ..Default::default() })`.

How the power state behaves:

- An output switched off without `wakeOnInput` comes back only when asked
  (`setPower('on')`, `wlopm --on`). With `wakeOnInput`, key presses, clicks,
  pointer motion, scrolling and touch wake it. Releasing a key does not, so the
  binding that switched the panels off does not switch them straight back on.
  Pressing a `'toggle'` binding while its panels are off just wakes them.
- Switching to another VT and back switches every output on, so an idle daemon
  that crashed cannot leave you in front of dark screens.
- Unplugging or disabling an output forgets its power state; it comes back on
  when it returns.
- While a panel is off nothing is rendered for it. Clients on it still get a
  frame callback once a second so they do not stall. Screenshots and
  screencasts of that output fail until it is back on (the screencast portal
  retries).

## Frame pacing (triple buffering)

`COMPOSITOR.rendering.framePacing` chooses how frames are paced on each output.

| Value | Behavior |
| --- | --- |
| `"throughput"` (default) | **Render-ahead / triple buffering.** While one frame waits for its vblank, the next one is already being rendered. |
| `"low-latency"` | A frame is rendered only after the previous one is on screen. |

```ts
// Every output (this is also the default when nothing is set)
COMPOSITOR.rendering.framePacing = "throughput";

// Per output: a function of the output, re-evaluated as the signals it reads change
COMPOSITOR.rendering.framePacing = (output) =>
  output.name.startsWith("DP-") ? "low-latency" : "throughput";
```

### Why render ahead

The kernel accepts one page flip per display at a time. With `"low-latency"`,
a frame can only start rendering once the previous flip has completed, so its
CPU **and** GPU work together must fit between that vblank and the driver's
commit deadline shortly before the next one. That is roughly 5 ms of an 8.3 ms
period at 120 Hz. A heavy frame (many windows, blur, shaders) that misses it
shows up one refresh late, and the animation stutters.

With `"throughput"`, the next frame starts as soon as the previous one has been
submitted and is held until the flip completes. This has two effects:

- **More time per frame.** A frame gets up to one extra refresh period.
- **Overlapping work.** One frame's GPU work overlaps the next frame's CPU work,
  so what has to fit in a period is the larger of the two rather than their sum.

### The cost: one frame of latency, only while frames are continuous

While frames are produced back to back (an animation, a drag, video), what you
see is one refresh later than with `"low-latency"`. That is about 8 ms at
120 Hz and about 17 ms at 60 Hz.

A frame that follows an idle period is never ahead of anything, so isolated
updates such as typing in a terminal are not delayed.

The hardware cursor keeps its freshness in both modes. While the pointer is
moving, a frame rendered ahead is still held until just before its vblank so
the cursor plane can carry the latest position. In that case less is gained
from rendering ahead.

### When the compositor always uses low latency

Whatever `framePacing` says, render-ahead is turned off for:

- **Tearing.** A game asking for immediate flips wants its newest frame on screen now.
- **Fullscreen fast path / direct scanout.** A game's own buffer would be held one frame longer.
- **Cursor-only updates.** Moving the pointer over a still screen re-renders nothing.

The environment variable `SHOJI_RENDER_AHEAD=0` turns render-ahead off
everywhere (handy for comparing).

The compositor logs `frame pacing changed` with the per-output result whenever
the effective setting changes, for example after a hot reload.

### Picking a mode

```ts
// Battery: prefer smoothness. AC: prefer latency.
COMPOSITOR.rendering.framePacing = () =>
  onBattery() ? "throughput" : "low-latency";
```

`onBattery` stands for any signal you maintain yourself (for example, from a
`createPoll` that reads `/sys/class/power_supply`).

Animations and polls run on each output's own frame clock in both modes. Their
timing is the same whichever you pick; only the latency of what reaches the
screen changes (see [Frame Timing & Polls](./timing.md)).

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
| `setPower(power, options?)` | switch panels on or off (above) |

`OutputInfo` includes `name`, `enabled`, `resolution` (`{width, height,
refreshRate}`), `position` (`{x, y}`), `scale`, `transform`, `subpixel`,
`detectedSubpixel`, `availableModes`, and identification fields (`make`,
`model`, `serial`, `connector`).

`subpixel` is the layout currently advertised, and `detectedSubpixel` the one
the kernel reported, so a settings UI can show what a connector claims before
anything overrides it.

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
