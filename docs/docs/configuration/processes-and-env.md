---
sidebar_position: 6
---

# Processes & Environment

## Session logs

Session logs and Timescope profiles are stored in `$XDG_STATE_HOME/shoji_wm/logs/`,
with `~/.local/state/shoji_wm/logs/` as the default. An empty or relative
`XDG_STATE_HOME` is ignored. Set `SHOJI_LOG_DIR` before starting the compositor
to override the complete directory; a relative override is resolved from its
working directory. An empty override is ignored.

The current session log is `latest.log`. Rotation and retention apply within
the selected directory. The default-path change does not migrate or delete
files under `~/shoji_wm/logs/`. Set `SHOJI_LOG_DIR` to that path to keep using
it; normal rotation and retention then apply there.

## Spawning processes

`COMPOSITOR.process` launches external programs. There are three methods,
differing in how the compositor tracks the process.

| Method | Lifecycle | Use for |
| --- | --- | --- |
| `once(id, spec)` | Run once at startup | Daemons/agents that start with the session |
| `service(id, spec)` | Long-running, monitored & restarted | Watchers that must stay alive |
| `spawn(spec)` | Fire-and-forget, untracked | On-demand launches (keybindings) |

### `once`

Runs a command once at session startup.

```ts
COMPOSITOR.process.once('fcitx5', {
  command: 'fcitx5 -d',
  runPolicy: 'once-per-session',
});
```

`runPolicy` controls re-runs:

- `"once-per-session"` *(default)* — run a single time per login session.
- `"once-per-config-version"` — also re-run after the config changes (hot reload).

### `service`

Starts a long-running process the compositor monitors and restarts.

```ts
COMPOSITOR.process.service('cliphist-text', {
  command: ['wl-paste', '--type', 'text', '--watch', 'cliphist', 'store'],
  restart: 'on-exit',
});
```

`restart` policy: `"never"`, `"on-failure"` (restart only on non-zero exit), or
`"on-exit"` (always restart).

### `spawn`

Launches a process and forgets it — not tracked or restarted. Ideal inside
keybinding handlers.

```ts
COMPOSITOR.key.bind('terminal', 'Super+T', () => {
  COMPOSITOR.process.spawn({command: ['kitty']});
});
```

### The command spec

Every method takes a spec with a `command`, plus optional `cwd` and `env`:

| Field | Type | Meaning |
| --- | --- | --- |
| `command` | `string \| string[]` | The program to run (see below) |
| `cwd` | `string` | Working directory |
| `env` | `Record<string, string \| number \| boolean>` | Extra environment for this process |

**How `command` is interpreted:**

- A **single string** runs via `/bin/sh -lc <command>`, so shell features (pipes,
  redirection, `~`, env expansion) work:
  ```ts
  COMPOSITOR.process.spawn({command: 'hyprshot -m region --raw | swappy -f -'});
  ```
- A **string array** is exec'd directly with no shell — each element is one argv
  entry, taken literally (safer, no quoting pitfalls):
  ```ts
  COMPOSITOR.process.spawn({command: ['kitty', '--title', 'My Terminal']});
  ```

## Starting locked

`COMPOSITOR.session.startLocked = true` starts the compositor with the session
already locked: nothing but ext-session-lock surfaces is drawn, and input
reaches only them, until a locker locks and then unlocks. With automatic login,
the lock screen becomes the login screen and the desktop never shows first.

ShojiWM does not draw a lock screen itself, so start a locker from the config —
any ext-session-lock client works (Quickshell's `WlSessionLock`, hyprlock,
swaylock, ...):

```ts
COMPOSITOR.session.startLocked = true;
COMPOSITOR.process.once('locker', { command: ['hyprlock'] });
```

- Set it at the top level of the config. It is read once, when the config first
  loads; a hot reload never locks a running session.
- Until the locker connects, the screen stays black.
- A config that fails to load starts unlocked, since its locker would not start
  either.
- If the locker never shows up, `Ctrl+Alt+F<n>` still switches to another VT
  while locked.
- Only one client can hold the lock. Another client that asks to lock is
  refused, and so cannot unlock it. If the locker dies, the session stays
  locked until a new locker takes over.
- A Rust config does the same with `ConfigBuilder::start_locked(true)`.

## Environment variables

`COMPOSITOR.env` manages the environment inherited by processes the compositor
spawns. Changes affect processes started **after** the call; running processes
are unaffected unless you `publish`.

```ts
COMPOSITOR.env.set('QT_QPA_PLATFORM', 'wayland;xcb');

COMPOSITOR.env.apply({
  QT_IM_MODULE: 'fcitx',
  XMODIFIERS: '@im=fcitx',
  MOZ_ENABLE_WAYLAND: 1,
});
```

| Method | Meaning |
| --- | --- |
| `set(key, value)` | Set one variable (value may be string/number/boolean) |
| `unset(key)` | Remove a variable |
| `get(key)` | Read the current value (`string \| undefined`) |
| `apply(values)` | Bulk set; pass `null`/`undefined` as a value to unset that key |
| `publish(keys?)` | Broadcast the current env to running services; all keys if omitted |

:::tip
Set environment variables **before** spawning the processes that need them. The
default config sets the Qt platform and input-method variables near the top,
then launches the relevant apps afterward.
:::
