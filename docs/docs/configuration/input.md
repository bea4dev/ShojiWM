---
sidebar_position: 4
---

# Input devices

`COMPOSITOR.input` configures keyboards, pointers (mice), and touchpads. As with
outputs, you register a factory that the compositor calls whenever the set of
input devices changes; you mutate the draft it passes in.

```ts
COMPOSITOR.input.configure((input, context) => {
  // Global defaults applied to all matching devices
  input.global = {
    touchpad: {
      tapToClick: true,
      naturalScroll: true,
      scrollMethod: 'twoFinger',
      disableWhileTyping: true,
      scrollFactor: 0.3,
    },
    pointer: {
      pointerAccel: 0.0,
      accelProfile: 'flat',
    },
    keyboard: {
      options: 'caps:ctrl_modifier',
      repeatRate: 60,
      repeatDelay: 250,
    },
  };

  // Per-device override, keyed by device name
  input.device['Razer Razer Blade Keyboard'] = {
    keyboard: {layout: 'us'},
  };
});
```

`input.global` applies to every device of that kind. `input.device[name]`
overrides settings for one specific device (set it to `null` to clear an
override). Each value is an `InputDeviceConfig` with optional `keyboard`,
`pointer`, and `touchpad` sub-objects.

## Keyboard settings

`keyboard` — an `InputDeviceConfig.keyboard` object. The `rules`/`model`/
`layout`/`variant`/`options` fields are standard XKB settings.

| Field | Type | Meaning |
| --- | --- | --- |
| `layout` | `string` | XKB layout, e.g. `"us"`, `"jp"`, `"de"` |
| `variant` | `string` | Layout variant, e.g. `"dvorak"` |
| `options` | `string` | XKB options, e.g. `"caps:ctrl_modifier"`, `"ctrl:nocaps"` |
| `rules` | `string` | XKB rules set |
| `model` | `string` | Keyboard model |
| `repeatRate` | `number` | Key repeats per second |
| `repeatDelay` | `number` | Delay (ms) before key repeat starts |
| `numLock` | `"on" \| "off" \| "restore"` | Num Lock state at session start (see below) |

```ts
input.global = {
  keyboard: {layout: 'us', options: 'caps:ctrl_modifier', repeatRate: 60, repeatDelay: 250},
};
```

### Num Lock at session start

`numLock` sets the Num Lock state once per session, when the compositor first
applies a config with it. Later config reloads and new keyboards do not change
it, so a toggle you make with the key stays. A keymap change (a reload, or
another `layout`) keeps the current state.

- `"on"` / `"off"`: start with Num Lock on or off.
- `"restore"`: start with the state the previous session ended with. The
  compositor saves each change to `$XDG_STATE_HOME/shoji_wm/num-lock`
  (`~/.local/state/shoji_wm/num-lock` when `XDG_STATE_HOME` is unset). If the
  file does not exist yet, Num Lock stays as it is.

Without `numLock`, the compositor does not touch Num Lock. Only
`input.global.keyboard.numLock` is read: all keyboards share one Num Lock
state, so a per-device value has no effect.

```ts
input.global = {
  keyboard: {numLock: 'restore'},
};
```

## Reading the active keyboard layout

Subscribe to the seat's actual XKB layout from TypeScript:

```ts
const unsubscribe = COMPOSITOR.event.onKeyboardLayoutChange((event) => {
  console.log(event.index, event.name); // 0, "English (US)"
  // Write your panel's JSON status or broadcast through your IPC server here.
});
```

`index` is the zero-based XKB layout index and `name` is its XKB display name.
The event contains no key events or typed text. Register during config loading
or `onEnable` to receive the initial layout on the first scheduler tick after
enable, including after a config reload. Later ticks report changes to either
field, including keymap changes; unchanged layouts emit nothing. Multiple
changes before a tick are coalesced to the latest layout. The scheduler samples
the seat at its existing cadence (up to 250 ms when idle).

Registration returns an unsubscribe function. The listener is synchronous; keep
it short and handle errors from your chosen JSON/IPC transport. The compositor
does not choose a transport or write a status file. Configurations using the
earlier draft's `shojiwm-$WAYLAND_DISPLAY-keyboard.json` must now publish that
file from TypeScript or migrate their panel to IPC.

## The laptop lid

`COMPOSITOR.event.onLidSwitch` reports the lid closing and opening, so a config
can switch the built-in panel off while the lid is down:

```ts
COMPOSITOR.event.onLidSwitch(({closed}) => {
  COMPOSITOR.output.setPower(closed ? 'off' : 'on', {output: 'eDP-1'});
});
```

Like the layout above, it arrives on a scheduler tick: the current state on the
first tick after enable or a reload, then each change. Nothing is reported
until the lid switch first reports, so a machine without one never sees the
event; a close and reopen between two ticks are coalesced. Registration returns
an unsubscribe function. In a Rust config it is
`COMPOSITOR.event.on_lid_switch(|lid| ...)`, delivered at once, and
`COMPOSITOR.input.lid_switch()` is a signal holding the last state (`None`
before the first report).

`initial` is `true` when the event tells where the lid already was rather than
a movement: the state the switch reported when it came up (at startup, or back
from another VT), or the state a reloaded config starts with. Some laptops'
switches report closed this way while the lid is open, so start actions only on
events with `initial: false`.

These events only report the lid: systemd-logind still suspends on it as
`logind.conf` says, in no defined order with the config. To decide what the lid
does, take it from logind; see [Power and the lid](power.md).

## Pointer (mouse) settings

`pointer` — an `InputDeviceConfig.pointer` object.

| Field | Type | Meaning |
| --- | --- | --- |
| `pointerAccel` | `number` | Acceleration speed, `-1.0`…`1.0` |
| `accelProfile` | `"adaptive" \| "flat"` | Acceleration curve (`"flat"` = 1:1, no accel) |
| `naturalScroll` | `boolean` | Reverse scroll direction |
| `leftHanded` | `boolean` | Swap left/right buttons |
| `middleEmulation` | `boolean` | Emulate middle click via left+right |

```ts
input.global = {pointer: {accelProfile: 'flat', pointerAccel: 0.0}};
```

## Touchpad settings

`touchpad` — a `TouchpadInputConfig`. It **extends the pointer settings above**,
so every `pointer` field is also valid here, plus:

| Field | Type | Meaning |
| --- | --- | --- |
| `tapToClick` | `boolean` | Tap the pad to click |
| `tapButtonMap` | `"leftRightMiddle" \| "leftMiddleRight"` | Multi-finger tap → button mapping |
| `clickMethod` | `"buttonAreas" \| "clickfinger"` | How physical clicks map to buttons |
| `scrollMethod` | `"none" \| "twoFinger" \| "edge" \| "onButtonDown"` | How scrolling is triggered |
| `scrollFactor` | `number` | Scroll speed multiplier |
| `disableWhileTyping` | `boolean` | Ignore the pad while typing |

…plus all pointer fields (`pointerAccel`, `accelProfile`, `naturalScroll`,
`leftHanded`, `middleEmulation`).

```ts
input.global = {
  touchpad: {
    tapToClick: true,
    naturalScroll: true,
    scrollMethod: 'twoFinger',
    scrollFactor: 0.3,
    disableWhileTyping: true,
  },
};
```

## Reading and targeting devices

The factory's second argument (and the controller itself) exposes the connected
devices, so you can apply settings conditionally by kind.

| Member | Returns |
| --- | --- |
| `devices` | `InputDeviceInfo[]` |
| `current` | `Record<string, InputDeviceInfo>` |
| `get(key)` | `InputDeviceInfo \| undefined` |
| `find(predicate)` | first matching device |
| `configure(factory)` | register a config factory |
| `reconfigure()` | re-run all factories now |

Each `InputDeviceInfo` has `key`, `name`, optional `vendor`/`product`, and a
`kind` flag object (`keyboard`, `pointer`, `touchpad`, `touch`, `tabletTool`,
`tabletPad`, `gesture`, `switch`).

```ts
COMPOSITOR.input.configure((input, ctx) => {
  for (const device of ctx.devices) {
    if (device.kind.touchpad) {
      input.device[device.key] = {touchpad: {scrollMethod: 'twoFinger'}};
    }
  }
});
```
