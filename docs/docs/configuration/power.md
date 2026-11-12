---
sidebar_position: 4.7
---

# Power and the lid

`COMPOSITOR.power` asks systemd-logind to suspend, hibernate and the like, tells
the config when the system comes back, and lets the config handle the laptop lid
instead of logind. Only the TTY backend talks to logind; in a nested session
these calls do nothing.

## Who handles the lid

logind reads the lid switch itself and acts on it as `logind.conf` says
(`HandleLidSwitch=suspend` by default). ShojiWM hears of the lid through
libinput at the same moment, in no defined order. So a config that only listens
to [`onLidSwitch`](input.md#the-laptop-lid) cannot keep logind from suspending,
and may not run before it does.

To decide what the lid does, the config takes it from logind:

```ts
COMPOSITOR.power.takeOver({lid: true});
```

ShojiWM then holds a `handle-lid-switch` inhibitor, logind leaves the lid alone,
and closing it does nothing until the config does something. The inhibitor is a
file descriptor: if ShojiWM goes away, logind has the lid back at once. Declare
it again on every reload, as a config does everything in its setup: a reloaded
config that does not, or that fails to load, gives the lid back to logind.

## The default config's lid policy

The default config takes the lid and sets what closing it does the way Windows
and KDE Plasma do, once for battery and once for external power. It is ordinary
config code, `bindLidPolicy` in `packages/config/src/lid-policy.ts`, so copy or
change it as you like:

```ts
bindLidPolicy({
  onBattery: 'logind',
  onExternalPower: 'logind',
});
```

`"logind"` does what `logind.conf` says in the situation (`HandleLidSwitch`,
`HandleLidSwitchExternalPower`, and `HandleLidSwitchDocked` while another
display is on), so out of the box the lid behaves as before. Set an action to
choose yourself:

```ts
bindLidPolicy({
  onBattery: 'hibernate',
  onExternalPower: 'suspend',
  onSleep: async () => {
    // Bring up the screen locker before the machine sleeps.
  },
  onResume: ({lidClosed}) => {
    // Back from a suspend or hibernation.
  },
});
```

| Option | Default | Meaning |
| --- | --- | --- |
| `onBattery` | `"logind"` | What closing the lid does on battery. |
| `onExternalPower` | `"logind"` | What closing the lid does on external power. |
| `onSleep` | none | Called before the policy puts the machine to sleep (`suspend`, `hibernate`, `hybrid-sleep`, `suspend-then-hibernate`, `sleep`) with `{action, externalPower, externalDisplay}`. A returned promise is waited for, at most `sleepCallbackTimeoutMs`; if it throws, the machine sleeps anyway. |
| `onResume` | none | Called after every resume, whoever started the sleep, with `{lidClosed}`. |
| `internalPanelOff` | `true` | Switch the built-in panel off (DPMS) while the lid is closed and another display is on, and back on when the lid opens or that display goes. |
| `holdoffMs` | `30000` | For this long after the policy starts and after a resume, closing the lid does nothing (logind's `HoldoffTimeoutSec`). |
| `sleepCallbackTimeoutMs` | `5000` | The longest wait for `onSleep` (logind's `InhibitDelayMaxSec`). |

An action is a [logind action](#power-actions), `"ignore"`, `"logind"`, or a
function that receives `{externalPower, externalDisplay}` and decides itself.
"Another display" is any enabled output that is not a built-in panel (`eDP`,
`LVDS`, `DSI`) or a headless output. Unplugging the last other display while the
lid is shut runs the action for the new situation, as logind does.
`bindLidPolicy` returns a function that gives the lid back.

### Broken lid switches

Some laptops' lid switches stick at "closed". A policy that suspends whenever
the switch says closed would put such a machine to sleep right after boot, and
again right after every resume. The policy only runs an action when the lid
moves:

- A lid that is closed when the switch comes up (at startup, or back from
  another VT) or when a reloaded config starts does nothing: the lid event
  arrives with `initial: true`. Only the built-in panel follows it, so a laptop
  booted shut on a dock keeps its panel dark.
- A resume restarts the holdoff, and a lid that still reads closed after it runs
  nothing until it moves again.
- libinput reports the lid open as soon as a key on the built-in keyboard is
  pressed, so a stuck switch recovers when someone types.

For a machine libinput does not know about, a local quirk tells libinput not to
trust the switch's state at startup (`unreliable`), or to write "open" back to
the kernel on the first key press (`write_open`), which also corrects logind:

```ini
# /etc/libinput/local-overrides.quirks
[My laptop's lid switch]
MatchName=*Lid Switch*
MatchDMIModalias=dmi:*svnVENDOR:pnMODEL:*
AttrLidSwitchReliability=write_open
```

## Power actions

```ts
COMPOSITOR.power.suspend();
COMPOSITOR.power.hibernate();
COMPOSITOR.power.perform('suspend-then-hibernate');
```

`perform` takes logind's action names: `suspend`, `hibernate`, `hybrid-sleep`,
`suspend-then-hibernate`, `sleep` (whichever sleep logind is set up for, or a
suspend before systemd 256), `lock` (asks every session's screen locker to
lock, as `loginctl lock-sessions`), `poweroff`, `reboot` and `halt`. They are
not interactive: when polkit would ask for a password, logind refuses and the
refusal is logged.

## State and resume

```ts
COMPOSITOR.power.onStateChange(({externalPower, logindLidActions}) => {
  console.log(externalPower ? 'on the charger' : 'on battery', logindLidActions);
});
COMPOSITOR.power.onResume(({lidClosed}) => {
  // Back from a suspend or hibernation, whoever started it.
});
```

`COMPOSITOR.power.state` holds the latest state: `externalPower` (whether the
machine runs on external power, decided as systemd does: an adapter online, or
no adapter at all) and `logindLidActions` (logind's three lid settings).
Either is `undefined` until known. The state arrives on a scheduler tick, like
the lid, and before the lid event of the same tick.

`onResume` fires after every resume. `lidClosed` is what the lid switch reported
last, which a stuck switch may still say is closed. Whether the resume or the
lid opening comes first is not defined, so make both handlers bring things to
the current state rather than depend on the order.

## In a Rust config

The same API, in `COMPOSITOR.power`:

```rust
COMPOSITOR.power.take_over(PowerTakeOverOptions { lid: true });
COMPOSITOR.power.on_resume(|resume| println!("resumed, lid {:?}", resume.lid_closed));
let power = COMPOSITOR.power.state(); // a signal
```

`take_over`, `suspend`, `hibernate` and `perform` match the TypeScript methods,
and `state()` is a signal holding the latest state (`None` before the first
report). Events are delivered at once rather than on a scheduler tick.

The Rust example config has the same lid policy in
`src/shojiwm_rs/examples/default_config/lid_policy.rs`, with `on_battery`,
`on_external_power`, `on_sleep` and `on_resume`. A Rust config runs
synchronously, so `on_sleep` returns before the machine sleeps rather than being
waited for.
