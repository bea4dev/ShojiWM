---
sidebar_position: 12
---

# KDE Plasma シェル

ShojiWM は KDE の `plasmashell` を動かせます。KDE のタスクバー、Kickoff、
システムトレイ、カレンダー、通知、デスクトップはそのまま使え、コンポジターだけが
KWin から ShojiWM に置き換わります。

```
┌────────────────────────────────────────────────┐
│ plasmashell: taskbar, Kickoff, tray, calendar, │
│ notifications, desktop, Pager                  │
├────────────────────────────────────────────────┤
│ ShojiWM (instead of KWin): your config decides │
│ layout, decorations, switchers and effects     │
└────────────────────────────────────────────────┘
```

ShojiWM は plasmashell が必要とする KDE のプロトコルを話します。

| プロトコル | plasmashell の用途 |
| --- | --- |
| `org_kde_plasma_window_management` (v18) | タスクバーがウィンドウを一覧・グループ化し、アクティブ化・最小化・クローズします。 |
| `org_kde_plasma_shell` (v8) | Kickoff、ポップアップ、OSD、通知がロールと位置を持ちます。 |
| `org_kde_plasma_virtual_desktop_management` (v2) | Pager がワークスペースを仮想デスクトップとして表示します。 |
| `xdg_toplevel_tag_manager_v1` | 受け取って無視します。これがないと Spectacle の範囲選択モードがクラッシュします。 |

残りは config の役割です。plasmashell を起動し、そのウィンドウをレイアウトから
外し、（必要なら）ワークスペースをデスクトップに対応付けます。以下で順に説明します。

## 必要なもの

- KDE Plasma 6（`plasmashell`、`kglobalacceld`、`qdbus6`）
- 後述の KWin の代替サービスを使う場合: `python-gobject` と `grim`

## plasmashell を起動する

plasmashell は KDE のセッションを前提にしています。KDE の環境変数を付けて
サービスとして起動します。Kickoff などのグローバルショートカットには KDE の
ショートカットデーモン `kglobalacceld` も必要です。通常は KWin が
`kglobalacceld` に Wayland バックエンドを提供しますが、ShojiWM では Xwayland 経由の
X11 バックエンドで動かします。

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

| 変数 | 理由 |
| --- | --- |
| `XDG_CURRENT_DESKTOP=KDE` | plasmashell と KDE のライブラリは KDE 上でのみ KDE 向けの挙動を読み込みます。 |
| `XDG_MENU_PREFIX=plasma-` | Kickoff は `plasma-applications.menu` からアプリ一覧を読みます。これがないとメニューが空になります。 |
| `KDE_FULL_SESSION=true` | KDE のライブラリが KDE 専用の挙動（パネルの色に合わせたトレイアイコンなど）を使います。 |
| `KDE_SESSION_VERSION=6` | 動いている Plasma のバージョンを KDE のライブラリに伝えます。 |

`/usr/lib/kglobalacceld` は Arch Linux のパスです。他のディストリビューションでは
`/usr/libexec` にあることがあります。

## plasmashell のウィンドウをレイアウトから外す

plasmashell のパネル、ポップアップ、通知は普通の Wayland ウィンドウです。
`window.plasma` で区別できます。plasmashell が要求したロールと位置を持ち、
それ以外のウィンドウでは `undefined` です。

| `role` | ウィンドウ |
| --- | --- |
| `desktop` | 壁紙とデスクトップアイコン |
| `panel` | パネル（タスクバー） |
| `appletpopup` | Kickoff、カレンダー、トレイのポップアップ |
| `notification`、`criticalnotification`、`onscreendisplay` | 通知と OSD |
| `tooltip` | ツールチップ |
| `normal` | 通常はアプリ。タスクバーのグループ化されたウィンドウ一覧も `normal` ですが、位置を指定します。 |

一般的な config では、これらのウィンドウにタイトルバーを付けず、ウィンドウ
マネージャーに渡さず、plasmashell が指定した位置に通常のウィンドウより上に
描きます。

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

他のウィンドウイベント（`onClose`、`onFocus`、移動やリサイズの要求）も同じように
振り分け、ウィンドウマネージャーがシェルのウィンドウを扱わないようにします。

KWin は plasmashell のポップアップの下に影を描きます（`org_kde_kwin_shadow`）。
ShojiWM はこのプロトコルを話さないので、必要なら `WindowBorder` の `boxShadow` で
影を描いてください。

## ワークスペースを Pager に表示する

`COMPOSITOR.workspace.configure` は省略可能な `virtualDesktops` を受け取ります。
各デスクトップの `id` はワークスペースの id です。Pager のクリックは
`workspace.event.onActivate` に届きます。Pager やタスクバーがウィンドウを別の
デスクトップへ移すときは、イベントに `windowId` も入ります。

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

KDE の Pager はデスクトップが1つしかないと自分を隠します。空でもいくつかの
デスクトップを一覧に残してください。

## タスクバーへの最小化

タスクバーは各ウィンドウのエントリの位置をコンポジターに伝えます。ShojiWM は
これを `window.minimizedGeometry`（グローバル論理 px）として公開するので、最小化
アニメーションでウィンドウをタスクバーのアイコンへ飛ばせます。

## Super キーで Kickoff を開く

plasmashell の D-Bus 呼び出し `activateLauncherMenu` は KWin の下でしか動きません。
代わりに kglobalaccel で Kickoff 自身のグローバルショートカットを呼びます。

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

## KWin の D-Bus 代替サービス（任意）

KDE のいくつかの機能は D-Bus で KWin を呼びます。そのバス名の持ち主がいないと、
代替動作になるか失敗します。
[`examples/kde-plasmashell`](https://github.com/bea4dev/ShojiWM/tree/main/examples/kde-plasmashell)
にある2つの小さな Python サービスがこれらの名前を持ち、IPC ソケット
（`createIpcServer`）経由で config とやり取りします。

| サービス | バス名 | 動くようになるもの | config が応答するもの |
| --- | --- | --- | --- |
| `window-view-dbus.py` | `org.kde.KWin.Effect.WindowView1` | グループ化されたタスクバーアイコンのクリックで、テキスト一覧ではなくウィンドウを並べて表示します。 | `{ windowIds }` 付きの `windowView.activate` |
| `screenshot-dbus.py` | `org.kde.KWin.ScreenShot2` | Spectacle（Print Screen）。`grim` で撮影します。 | `workspaces.get` と `windows.rect` |

```ts
for (const name of ['window-view-dbus', 'screenshot-dbus']) {
  COMPOSITOR.process.service(name, {
    command: ['python3', `${configDir}/helpers/${name}.py`, defaultSocketPath()],
    restart: 'on-exit',
  });
}
```

## 制限

- KWin のエフェクトと KWin スクリプトは動きません。エフェクトはすべて config が描きます。
- KDE のセッションマネージャー（`ksmserver`）は動かないので、Plasma の「ログアウト」
  ボタンは何もしません。代わりに `shoji_wm --quit` をキーに割り当ててください。
- Spectacle の「カーソル下のウィンドウ」モードはアクティブなウィンドウを撮影し、
  KWin 経由の画面録画は動きません。
- Plasma 6 でのみテストしています。
