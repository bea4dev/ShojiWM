---
sidebar_position: 4.8
---

# GPUs

`COMPOSITOR.gpu` lists the GPUs of the seat, picks the one everything is
composited on (the **render GPU**), and gives the environment that makes a
program render on a given GPU. Only the TTY backend has GPUs to report; in a
nested session the list stays empty and `selectRender` is never asked.

## How ShojiWM uses several GPUs

Every GPU is opened, also the ones with nothing connected yet, so a display
plugged into any of them later works. Everything is composited on **one** GPU,
the render GPU, and client buffers are allocated for it. A display on another
GPU still shows the desktop: its frame is composited on the render GPU into a
LINEAR buffer (the layout every vendor can read) and that GPU only puts the
buffer on screen.

Without a choice from the config, ShojiWM picks the render GPU itself, in this
order:

1. the GPU driving the built-in panel (`eDP`, `LVDS`, `DSI`); on a hybrid
   laptop, the integrated one,
2. the GPU the firmware showed the boot screen on (`bootVga`),
3. the GPU with the most displays connected.

## Reading the GPUs

```ts
import {COMPOSITOR} from 'shoji_wm';

COMPOSITOR.gpu.onChange((gpus) => {
  for (const gpu of gpus) {
    console.log(gpu.id, gpu.name, gpu.driver, gpu.outputs, gpu.isRender);
  }
});
```

| Member | Returns |
| --- | --- |
| `list` | `GpuInfo[]` — every GPU, ordered by `id` |
| `render` | `GpuInfo \| undefined` — the render GPU |
| `get(id)` | `GpuInfo \| undefined` |
| `onChange(listener)` | called with every GPU when one is added or removed, a display is connected or disconnected, and first after enable/reload |
| `selectRender(resolver)` | choose the render GPU (below) |
| `offloadEnv(gpu)` | environment to run a program on `gpu` (below) |

`list` is empty until the first report after `onEnable`; listen with
`onChange` rather than reading it at the top level of the config.

`GpuInfo`:

| Field | Meaning |
| --- | --- |
| `id` | Where the GPU sits, e.g. `"pci-0000:03:00.0"` (udev's `ID_PATH`). Stays the same across reboots: **name GPUs by this.** |
| `dynamicCardId` | `"card1"`. Numbered in probe order, so it can change between boots and when a GPU is plugged in again. For logs and display only. |
| `renderNode` | `"/dev/dri/renderD129"` (numbered like `dynamicCardId`), or `null` |
| `vendor` | `"intel"`, `"amd"`, `"nvidia"` or `"other"` |
| `vendorId`, `deviceId` | PCI ids; 0 when the GPU is not on PCI |
| `driver` | Kernel driver: `"i915"`, `"xe"`, `"amdgpu"`, `"nvidia"`, `"nouveau"`, ... |
| `name` | Model name from the hardware database, e.g. `"Navi 32 [Radeon RX 7800 XT]"` |
| `bootVga` | The firmware showed the boot screen on it (usually the integrated GPU of a hybrid laptop) |
| `removable` | The kernel reports it as removable (a Thunderbolt eGPU and the like) |
| `outputs` | Connectors with a display connected, named like outputs (`"DP-1"`) |
| `isRender` | Everything is composited on it |
| `inUse` | ShojiWM opened it; `SHOJI_TTY_DRM_DEVICE` can leave GPUs out |

There is no "integrated" flag: Linux has no reliable one (an AMD APU looks
like any other GPU), so tell GPUs apart by `bootVga`, `driver`, `vendor`,
`outputs` or `id`.

## Choosing the render GPU

```ts
COMPOSITOR.gpu.selectRender((gpus) =>
  gpus.find((gpu) => gpu.vendor === 'amd' && !gpu.bootVga) ?? 'auto',
);
```

The resolver returns a `GpuInfo`, an `id` (also accepted: `"card1"`,
`"/dev/dri/card1"`, a render node, or the `DRI_PRIME` form
`"pci-0000_03_00_0"`), or `"auto"` / nothing for ShojiWM's own pick.

- It is asked **once, at startup, before `onEnable` and before any GPU is
  opened**, so register it at the top level of the config. A change takes
  effect at the next start; hot reload does not switch GPUs.
- It is asked **only when there is more than one GPU**. On a single-GPU
  machine nothing changes, not even when the config is loaded.
- Inside the resolver, `isRender` marks the GPU ShojiWM would pick on its
  own and `inUse` the GPUs it is going to open.
- A GPU that is not going to be opened is opened when chosen, except when
  `SHOJI_TTY_DRM_DEVICE` names the GPUs to open: that list wins, and the
  choice is ignored with a warning.
- A choice that matches no GPU, or a resolver that throws, falls back to the
  automatic pick and is shown as a config error.
- If the chosen GPU fails to initialize (its driver cannot be loaded, for
  example), the next one takes over, so a bad choice cannot leave you without
  a session.

The environment variable `SHOJI_RENDER_GPU` (same values as the resolver's
answer) wins over the config, which helps to try a GPU without editing the
config; `SHOJI_RENDER_GPU=auto` ignores the config's choice:

```sh
SHOJI_RENDER_GPU=pci-0000:03:00.0 shoji_wm --tty
```

### What it costs

Displays on other GPUs get their frames copied over every frame (a LINEAR
copy across PCIe). With the render GPU on a hybrid laptop's discrete GPU, the
built-in panel becomes such a display, and the discrete GPU can no longer
power down while the session runs.

The proprietary NVIDIA driver cannot render into a LINEAR dmabuf. With NVIDIA
as the render GPU, such frames are rendered into a LINEAR GBM surface instead:
the compositing runs in the GPU's own memory layout, and the driver copies the
frame into the LINEAR buffer once, when it is swapped, as NVIDIA's own PRIME
render offload does. The session log then says `rendering cross-gpu frames into
a LINEAR gbm surface`.

The GPU driving such a display waits for the render GPU's frame on its own
(through the frame's fence), so the CPU moves on at once and
[`framePacing = "throughput"`](outputs.md#frame-pacing-triple-buffering) keeps
overlapping one frame's GPU work with the next frame's CPU work. That needs
`EGL_ANDROID_native_fence_sync` on both drivers; without it the CPU waits for
the render GPU, which still works but stalls the event loop for that time. The
session log says which of the two it uses (`cross-gpu frames: ...`). Setting
`SHOJI_CROSS_GPU_CPU_WAIT` forces the CPU wait, should a pair of drivers
mishandle each other's fences.

## Running a program on a given GPU

`offloadEnv(gpu)` returns the environment for PRIME render offload, for
`COMPOSITOR.process.spawn`:

```ts
COMPOSITOR.key.bind('steam', 'Super+G', () => {
  const dgpu = COMPOSITOR.gpu.list.find((gpu) => !gpu.bootVga);
  COMPOSITOR.process.spawn({
    command: 'steam',
    env: dgpu ? COMPOSITOR.gpu.offloadEnv(dgpu) : {},
  });
});
```

| Driver | Environment |
| --- | --- |
| `nvidia` (proprietary) | `__NV_PRIME_RENDER_OFFLOAD=1`, `__GLX_VENDOR_LIBRARY_NAME=nvidia`, `__VK_LAYER_NV_optimus=NVIDIA_only` |
| any Mesa driver (`i915`, `xe`, `amdgpu`, `nouveau`, ...) | `DRI_PRIME=<id as a udev tag>`, `MESA_VK_DEVICE_SELECT=<vendor>:<device>` |

These only pick the GPU a program renders on. Its windows are still
composited on the render GPU.

## Rust configs

```rust
COMPOSITOR.gpu.select_render(|gpus| {
    gpus.iter()
        .find(|gpu| gpu.vendor == GpuVendor::Amd && !gpu.boot_vga)
        .map_or(RenderGpuChoice::Auto, |gpu| RenderGpuChoice::Gpu(gpu.id.clone()))
});

effect(|| {
    for gpu in COMPOSITOR.gpu.list().get() {
        println!("{} {:?}", gpu.id, gpu.name);
    }
});
```

`list()` is a signal, `render()` and `get(id)` read it, and
`offload_env(&gpu)` returns the same environment as key/value pairs.
