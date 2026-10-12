---
sidebar_position: 12
---

# KDE Plasma Shell

ShojiWM can host KDE's `plasmashell`, so you keep the KDE taskbar, Kickoff,
the system tray, the calendar, notifications and the desktop, and ShojiWM
replaces KWin as the compositor.

```
┌────────────────────────────────────────────────┐
│ plasmashell: taskbar, Kickoff, tray, calendar, │
│ notifications, desktop, Pager                  │
├────────────────────────────────────────────────┤
│ ShojiWM (instead of KWin): your config decides │
│ layout, decorations, switchers and effects     │
└────────────────────────────────────────────────┘
```

ShojiWM speaks the KDE protocols that plasmashell needs:

| Protocol | What plasmashell does with it |
| --- | --- |
| `org_kde_plasma_window_management` (v18) | The taskbar lists, groups, activates, minimizes and closes windows. |
| `org_kde_plasma_shell` (v8) | Kickoff, popups, OSDs and notifications get a role and a position. |
| `org_kde_plasma_virtual_desktop_management` (v2) | The Pager shows your workspaces as virtual desktops. |
| `xdg_toplevel_tag_manager_v1` | Accepted and ignored. Spectacle's region mode crashes without it. |

The rest is up to your config: start plasmashell, keep its windows out of
your layout, and (optionally) map workspaces to desktops. This page shows each
step.

## Requirements

- KDE Plasma 6 (`plasmashell`, `kglobalacceld`, `qdbus6`).
- For the optional KWin stand-ins below: `python-gobject` and `grim`.

## Start plasmashell

plasmashell expects a KDE session. Start it as a service with a KDE
environment. It also needs KDE's shortcut daemon, `kglobalacceld`, for global
shortcuts such as Kickoff's. KWin normally gives `kglobalacceld` a Wayland
backend; under ShojiWM it runs on its X11 backend through Xwayland.

```ts
COMPOSITOR.process.service('kglobalaccel', {
  command: [
    'env', '-u', 'WAYLAND_DISPLAY',
    'KGLOBALACCELD_PLATFORM=xcb', 'QT_QPA_PLATFORM=xcb', 'XDG_SESSION_TYPE=x11',
    '/usr/lib/kglobalacceld',
  ],
  restart: 'on-exit',
});

COMPOSITOR.process.service('plasmashell', {
  // Wait up to 3 s for kglobalaccel, so Kickoff keeps its shortcut.
  command: [
    'sh', '-c',
    'for i in 1 2 3 4 5 6; do qdbus6 org.kde.kglobalaccel >/dev/null 2>&1 && break; sleep 0.5; done; exec plasmashell --no-respawn',
  ],
  env: {
    XDG_CURRENT_DESKTOP: 'KDE',
    XDG_MENU_PREFIX: 'plasma-',
    KDE_FULL_SESSION: 'true',
    KDE_SESSION_VERSION: '6',
  },
  restart: 'on-exit',
});
```

| Variable | Why |
| --- | --- |
| `XDG_CURRENT_DESKTOP=KDE` | plasmashell and KDE libraries only load KDE behaviour on KDE. |
| `XDG_MENU_PREFIX=plasma-` | Kickoff reads its app list from `plasma-applications.menu`. Without it the menu is empty. |
| `KDE_FULL_SESSION=true` | KDE libraries apply KDE-only behaviour, such as panel-coloured tray icons. |
| `KDE_SESSION_VERSION=6` | Tells KDE libraries which Plasma version runs. |

`/usr/lib/kglobalacceld` is the Arch Linux path. Other distributions may
install it under `/usr/libexec`.

## Keep plasmashell's windows out of your layout

plasmashell's panels, popups and notifications are normal Wayland windows.
`window.plasma` tells them apart: it holds the role and position plasmashell
asked for, and is `undefined` for every other window.

| `role` | Windows |
| --- | --- |
| `desktop` | The wallpaper and desktop icons. |
| `panel` | Panels (the taskbar). |
| `appletpopup` | Kickoff, the calendar, tray popups. |
| `notification`, `criticalnotification`, `onscreendisplay` | Notifications and OSDs. |
| `tooltip` | Tooltips. |
| `normal` | Usually an app. The taskbar's list of grouped windows also uses `normal`, but sets a position. |

A typical config gives these windows no title bar, keeps them out of its
window manager and draws them where plasmashell asks, above normal windows:

```tsx
function isShellWindow(window: WaylandWindow): boolean {
  const plasma = window.plasma.peek();
  return plasma !== undefined && (plasma.role !== 'normal' || plasma.x !== undefined);
}

COMPOSITOR.window.decoration.configure((window, context) =>
  isShellWindow(window) ? { mode: 'client' } : { mode: context.clientPreference ?? 'server' },
);

COMPOSITOR.event.onOpen((window) => {
  if (isShellWindow(window)) {
    // Popups with a search field close when they lose focus.
    const role = window.plasma.peek()?.role;
    if (role === 'appletpopup' || role === 'normal') {
      window.focus();
    }
    return; // no tiling, no workspace
  }
  myWindowManager.onOpen(window);
});

const SHELL_Z_INDEX = { desktop: -2_100_000_000, panel: 2_050_000_000, popup: 2_100_000_000 };

COMPOSITOR.window.composition = (window) => {
  const plasma = window.plasma();
  if (plasma && isShellWindow(window)) {
    const rect = computed(() => ({
      x: plasma.x ?? window.position.x,
      y: plasma.y ?? window.position.y,
      width: window.position.width,
      height: window.position.height,
    }));
    const zIndex =
      plasma.role === 'desktop' ? SHELL_Z_INDEX.desktop
      : plasma.role === 'panel' ? SHELL_Z_INDEX.panel
      : SHELL_Z_INDEX.popup;
    return (
      <ManagedWindow rect={rect} zIndex={zIndex}>
        <ClientWindow />
      </ManagedWindow>
    );
  }
  return myComposition(window);
};
```

Route the other window events (`onClose`, `onFocus`, move and resize requests)
the same way, so your window manager never sees a shell window.

KWin draws a shadow under plasmashell's popups (`org_kde_kwin_shadow`), which
ShojiWM does not speak. Draw your own with a `boxShadow` on a `WindowBorder`
if you want one.

## Show workspaces in the Pager

`COMPOSITOR.workspace.configure` takes an optional `virtualDesktops`. Each
desktop's `id` is a workspace id. A click in the Pager arrives in
`workspace.event.onActivate`. When the Pager or the taskbar moves a window to
another desktop, the event also has `windowId`.

```ts
COMPOSITOR.workspace.configure(() => ({
  groups: myWorkspaceGroups(),
  virtualDesktops: {
    desktops: [
      { id: 'DP-1:1', name: 'Desktop 1', active: true, windowIds: ['0x1', '0x2'] },
      { id: 'DP-1:2', name: 'Desktop 2', windowIds: [] },
    ],
    rows: 1,
    // Really minimized windows. A window on another desktop is hidden too,
    // but must not look minimized in the taskbar.
    minimizedWindowIds: [],
  },
}));

COMPOSITOR.workspace.event.onActivate((event) => {
  if (event.windowId) {
    moveWindowToWorkspace(event.windowId, event.workspaceId);
  } else {
    activateWorkspace(event.workspaceId);
  }
});
```

KDE's Pager hides itself when there is only one desktop. Keep a few desktops
in the list even when they are empty.

## Minimize to the taskbar

The taskbar tells the compositor where each window's entry is. ShojiWM shows
it as `window.minimizedGeometry` (global logical px), so a minimize animation
can fly the window into its taskbar icon.

## Kickoff on the Super key

plasmashell's `activateLauncherMenu` D-Bus call only works under KWin. Fire
Kickoff's own global shortcut in kglobalaccel instead:

```ts
const KICKOFF_ID = 'panels().forEach(function (p) { p.widgets("org.kde.plasma.kickoff").forEach(function (w) { print(w.id + "\\n"); }); });';

function toggleKickoff() {
  COMPOSITOR.process.spawn({
    command: [
      'sh', '-c',
      'id=$(qdbus6 org.kde.plasmashell /PlasmaShell evaluateScript "$1" | head -n 1) && [ -n "$id" ] && qdbus6 org.kde.kglobalaccel /component/plasmashell org.kde.kglobalaccel.Component.invokeShortcut "activate widget $id"',
      'toggle-launcher', KICKOFF_ID,
    ],
  });
}

// A quick Super tap (see "Tap bindings").
COMPOSITOR.key.bind('launcher-tap', 'Super', toggleKickoff, {on: 'release', within: 300});
```

## KWin D-Bus stand-ins (optional)

Some KDE features call KWin over D-Bus. Without an owner of the bus name they
fall back or fail. Two small Python services in
[`examples/kde-plasmashell`](https://github.com/bea4dev/ShojiWM/tree/main/examples/kde-plasmashell)
own these names and talk to your config over its IPC socket
(`createIpcServer`):

| Service | Bus name | Makes this work | Your config answers |
| --- | --- | --- | --- |
| `window-view-dbus.py` | `org.kde.KWin.Effect.WindowView1` | A click on a grouped taskbar icon shows the windows side by side, not a text list. | `windowView.activate` with `{ windowIds }` |
| `screenshot-dbus.py` | `org.kde.KWin.ScreenShot2` | Spectacle (Print Screen). It captures with `grim`. | `workspaces.get` and `windows.rect` |

```ts
for (const name of ['window-view-dbus', 'screenshot-dbus']) {
  COMPOSITOR.process.service(name, {
    command: ['python3', `${configDir}/helpers/${name}.py`, defaultSocketPath()],
    restart: 'on-exit',
  });
}
```

## Limitations

- KWin effects and KWin scripts do not run. Your config draws every effect.
- KDE's session manager (`ksmserver`) does not run, so Plasma's "Log Out"
  button does nothing. Bind a key to `shoji_wm --quit` instead.
- Spectacle's "window under cursor" mode captures the active window, and
  screen recording through KWin does not work.
- Only tested with Plasma 6.
