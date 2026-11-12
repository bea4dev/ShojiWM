---
sidebar_position: 5
---

# キーバインドとポインター

## キーボードショートカット

`COMPOSITOR.key.bind(id, shortcut, handler, options?)` はコンポジターレベルの
キーボード、マウスのサイドボタン、マウスホイールのショートカットを登録します。

```ts
COMPOSITOR.key.bind('terminal', 'Super+T', () => {
  COMPOSITOR.process.spawn({command: ['kitty']});
});
```

| 引数 | 型 | 意味 |
| --- | --- | --- |
| `id` | `string` | 一意な名前（ヘルプ UI に表示）。同じ id で再登録すると上書き。 |
| `shortcut` | `string` | モディファイア＋キーの記法（例: `"Super+Shift+Left"`） |
| `handler` | `() => void` | ショートカット発火時に呼ばれる |
| `options` | `{on?: "press" \| "release", within?: number}` | 発火タイミング。デフォルトは `"press"`。`within` は下の「タップバインド」を参照 |

### ショートカットの記法

モディファイアとキーを `+` で組み合わせます。

- **モディファイア:** `Super`・`Ctrl`・`Shift`・`Alt`
- **キー:** 英字（`T`・`Q`）、矢印（`Left`・`Right`・`Up`・`Down`）、
  ファンクションキー（`F`）など
- **キーコード:** `code:<xkb キーコード>` は、レイアウトに関係なく物理キーを指します。
  コードは evdev コードに 8 を足した値で、`wev` が表示するものと同じです。数字段が
  Shift なしで記号を出すレイアウト（フランス語 AZERTY など）でも、`Super+code:10`
  から `Super+code:19` は 1 から 0 の刻印があるキーを指します。
- **マウスのサイドボタン:** `MouseBack`（`BTN_SIDE`、275）と `MouseForward`
  （`BTN_EXTRA`、276）。名前の大文字と小文字は区別せず、発火タイミングはデフォルトの
  `on: "press"` です。`on: "release"` のサイドボタンバインドは無効になります。
- **マウスホイール:** `WheelScrollUp`・`WheelScrollDown`・`WheelScrollLeft`・
  `WheelScrollRight`。ホイールショートカットはマウスホイールの段階的なスクロールで
  発火します。タッチパッドのスクロールは連続的なままで、通常どおりアプリケーションに
  届きます。発火タイミングはデフォルトの `on: "press"` です。

```ts
COMPOSITOR.key.bind('close', 'Super+Q', () => focused?.close());
COMPOSITOR.key.bind('move-tile-left', 'Super+Shift+Left', () => moveTile(-1));
COMPOSITOR.key.bind('screenshot', 'Super+P', () => {
  COMPOSITOR.process.spawn({command: 'hyprshot -m region --raw | swappy -f -'});
});
COMPOSITOR.key.bind('focus-left', 'Super+WheelScrollUp', () => focusTile(-1));
COMPOSITOR.key.bind('focus-right', 'Super+WheelScrollDown', () => focusTile(1));
COMPOSITOR.key.bind('focus-left-button', 'Super+MouseBack', () => focusTile(-1));
COMPOSITOR.key.bind('focus-right-button', 'Super+MouseForward', () => focusTile(1));
```

サイドボタンのバインドは、指定したモディファイアと完全に一致したときだけ発火します。
たとえば `Super+MouseForward` を登録しても、`MouseForward` 単体はフォーカス中の
アプリケーションやゲームでそのまま使えます。一致したバインドは押下とリリースの両方を
消費します。先にモディファイアを離しても同じで、モディファイア単体のタップバインドも
発火しません。セッションのロック中やモーダルな入力グラブの間は、サイドボタンの
ショートカットは動作しません。

### タップバインド（`on: "release"`）

モディファイア単体を `{on: "release"}` で登録すると **タップ** になります。キーを
離したときに発火しますが、その間に他のキーやボタンが押されていない場合に限ります。
デフォルト設定では、`Super` を素早くタップするとランチャーを開きつつ、`Super` を
他のショートカットのモディファイアとしても使えるようにこれを利用しています。

```ts
COMPOSITOR.key.bind('launcher-tap', 'Super', openLauncher, {on: 'release'});
```

1分を超えて押されたままのキーは、押されていないものとして扱います。コンポジターが
受け取れなかった解放（KVM の切り替えやキーボードの取り外し）のせいでタップバインドが
効かなくなり続けるのを防ぐためです。実際に1分を超えて押し続けているキーでも、
タップは取り消されません。

`within`（ミリ秒）を付けると、素早いタップのときだけ発火します。ドラッグや
[ゲームのポインターロックの無視](./cursor.md#アプリのポインターロックを無視する)で
モディファイアを長く押していた場合は、離してもランチャーが開きません。
デフォルト設定では 300ms にしています。

```ts
COMPOSITOR.key.bind('launcher-tap', 'Super', openLauncher, {on: 'release', within: 300});
```

### リピートとロック中のバインド

`repeat: true` を付けると、キーを押している間ハンドラーが繰り返し呼ばれます。
キーボードの `repeatDelay` の後、毎秒 `repeatRate` 回です（[入力](./input.md)を
参照）。他のキーを押すと止まります。`locked: true` を付けると、セッションが
ロックされている間も発火します。通常のバインドはロック画面に吸収されるので、
音量、明るさ、メディアキーに使ってください。どちらもキーとデフォルトの
`on: "press"` が必要で、マウスボタン、ホイール方向、タップには使えません。

```ts
COMPOSITOR.key.bind('volume-up', 'XF86AudioRaiseVolume', raiseVolume, {locked: true, repeat: true});
```

Rust では `bind_with` で同じオプションを渡します。

```rust
COMPOSITOR.key.bind_with(
    "volume-up",
    "XF86AudioRaiseVolume",
    KeyBindOptions::new().locked().repeat(),
    || COMPOSITOR.process.spawn(Command::exec(["wpctl", "set-volume", "@DEFAULT_SINK@", "5%+"])),
);
```

### 終了とリロード

`Super+Shift+Q`（終了）と `Super+Shift+R`（設定のリロード）はコンポジター組み込みで、
設定が壊れていても常に使えます。同じ2つの操作はコマンドラインからも行えるので、
ステータスバーやスクリプト、独自のキーバインドから呼び出せます。

```sh
shoji_wm --quit     # または -q
shoji_wm --reload   # または -r
```

```ts
COMPOSITOR.key.bind('reload', 'Super+Ctrl+R', () => {
  COMPOSITOR.process.spawn({command: ['shoji_wm', '--reload']});
});
```

コマンドは `SHOJIWM_SOCKET` が指すソケット経由で実行中のコンポジターに伝わります。
ShojiWM は起動するすべてのプロセスにこの変数を渡します。TTYセッション、または
`SHOJI_PUBLISH_ACTIVATION_ENV=1` でnestedでの公開を有効にした場合は、systemd/D-Busの
アクティベーション環境にも渡します。未設定の場合は `WAYLAND_DISPLAY` からソケットを求めます。

### 外部からキーを押す

`shoji_wm --key <chord>` は、実行中のコンポジターでキーの組み合わせを押して離します。
キーボードからの入力と同じ経路を通るので、キーバインディング、組み込みの
`Super+Shift+Q`/`Super+Shift+R`、そしてフォーカス中のウィンドウに届きます。
組み合わせはバインディングと同じ書式（`Super+Shift+Return`、`Ctrl+c`）です。キーは
現在のレイアウト上の keysym として探され、`code:<xkb キーコード>` でコードを直接指定
できます。そのレイアウトで Shift が必要な keysym には Shift が加わります。
`--hold <ms>` を付けると、その時間だけ押し続けてから離します。キーリピートの
確認に使えます。

```sh
shoji_wm --key Super+T
shoji_wm --key XF86AudioRaiseVolume --hold 800
printf 'key Super+T\n' | socat - UNIX-CONNECT:"$SHOJIWM_SOCKET"
printf 'key XF86AudioRaiseVolume 800\n' | socat - UNIX-CONNECT:"$SHOJIWM_SOCKET"
```

### 仮想キーボード

`wtype` などのツールは `zwp_virtual_keyboard_v1` を使い、独自のキーマップを
アップロードします。そのキーは通常のキーボードと同じ経路を通ります。
`wtype -M logo -k space -m logo` は `Super+space` のバインディングを、
`wtype -k F13` は `F13` のバインディングを発火させ、打ち込んだ文字は送られた
キーマップでフォーカス中のウィンドウに届くため、レイアウトにない文字も入力できます。
バインディングは、ツールが選んだキーコードに関係なく、そのキーマップの keysym で
照合されます。`code:<xkb keycode>` のバインディングは、同じ keysym を持つ
現在のレイアウトのキーと照合されます。仮想キーボードが入力している間、フォーカス中の
ウィンドウはそのキーマップを使い、別のキーボードからの次のキー入力かツールの終了で
元のレイアウトに戻ります。config に通知されるレイアウトは変わりません。

fcitx5 などのインプットメソッドはキーボードをグラブし、処理しなかったキーを自身の
仮想キーボードから送り返します。これらのキーはキーバインディングを通らず、現在の
レイアウトのままフォーカス中のウィンドウに直接届きます。元のキーはインプットメソッドに
届く前にすでにキーバインディングを通っているためです。

### 仮想ポインター

ShojiWM は `zwlr_virtual_pointer_manager_v1`（バージョン 2）を提供します。
`wlrctl pointer move 100 0`、`wlrctl pointer click left` やリモートデスクトップの
サーバーは、マウスと同じ経路でポインターを動かします。ホットコーナー、ポインター
領域、入力グラブ、`bindWindowMoveModifier` もこれを受け取ります。特定の出力向けに
作られた仮想ポインターは、絶対座標の移動をその出力に対応させます。
`axis_source` も `axis_discrete` も指定しないスクロールは、スクロール量 15 を
1 ノッチとするマウスホイールとしてクライアントに届きます。ソースを指定した
スクロールはそのソースのままです。`wlrctl pointer scroll` は `finger` を送るので、
クライアントはタッチパッドと同じく滑らかにスクロールします。

## ポインター

`COMPOSITOR.pointer` は、コンポジター自身が扱うマウス操作を設定します。（加速度・
スクロール方式などデバイスごとの調整は [`COMPOSITOR.input`](./input.md) を使います。）

### モディファイアでウィンドウを移動

`bindWindowMoveModifier(modifier)` を使うと、モディファイアを押しながらウィンドウ上の
どこをクリックしてもドラッグで移動できます――タイトルバーをつかむ必要はありません。

```ts
COMPOSITOR.pointer.bindWindowMoveModifier('Super');
```

:::tip
インタラクティブなリサイズの当たり判定は、ウィンドウごとに
`<WindowBorder interaction={{resizeHitArea: …}}>` の prop で設定します。
[SSD コンポーネント](./components.md#windowborder) を参照してください。
:::

### ホットコーナー

`bindHotCorner(id, corner, handler, options?)` は、ポインターが画面の角に届いたときに
`handler` を実行します。角の判定はコンポジター側で行うため、それ以外の場所での
ポインター移動にはコストがかかりません。

```ts
COMPOSITOR.pointer.bindHotCorner('overview', 'top-left', () => {
  openWindowSwitcher();
});
```

| オプション | 既定値 | 意味 |
| --- | --- | --- |
| `output` | 全出力 | この出力の角だけ |
| `size` | `1` | 角の正方形の一辺（論理px） |
| `delay` | `0` | 角にこの ms 留まってから `handler` を実行 |
| `pressure` | — | 入ったときではなく、角へこの px 押し込んだときに実行 |
| `leaveMargin` | `8` | この距離だけ離れるまで、角は再発火しません |
| `ignoreWhenFullscreen` | `true` | 全画面ウィンドウを表示中の出力では無効 |
| `ignoreWhileButtonHeld` | `true` | ドラッグ中は無効 |

本当の角だけが対象です。辺の先に別のモニターが続いている場合、ポインターはそのまま
隣へ移動するだけなので、その角は発火しません。同じ `id` で再登録すると置き換わり、
戻り値の関数で解除できます。

`pressure` は誤爆を防ぐための指定です。カーソルは画面の端で止まりますが、コンポジターには
その後もマウスの動きが届いています。「角へさらに 100px 押し込む」は意図しないと起きない
操作なので、通りがかりで発火しなくなります。

```ts
COMPOSITOR.pointer.bindHotCorner('overview', 'top-left', openOverview, {pressure: 100});
```

### ポインター領域

ホットコーナーはポインター領域の上に作られています。ポインター領域は出力を基準に置く
矩形で、ポインターの出入りを通知します。`defineRegion(id, options)` で定義し、
`COMPOSITOR.event.onPointerRegion` でイベントを受け取ります。

```ts
// 各出力の上端中央にある 400 × 2 px の帯
COMPOSITOR.pointer.defineRegion('top-strip', {
  anchor: 'top',
  size: {width: 400, height: 2},
  outerEdgesOnly: true,
});

COMPOSITOR.event.onPointerRegion((event) => {
  // { id, phase: "enter" | "leave" | "pressure", outputName, position, modifiers, timestamp }
  if (event.id === 'top-strip' && event.phase === 'enter') {
    showBar(event.outputName);
  }
});
```

| オプション | 既定値 | 意味 |
| --- | --- | --- |
| `size` | （必須） | `{width, height}`（論理px）。出力からはみ出た部分は切り捨て |
| `anchor` | `"top-left"` | `"top-left"`, `"top"`, `"top-right"`, `"left"`, `"center"`, `"right"`, `"bottom-left"`, `"bottom"`, `"bottom-right"`。領域の同名の点を出力の同名の点に合わせます |
| `offset` | `{x: 0, y: 0}` | 配置後の移動量（x は右、y は下が正） |
| `output` | 全出力 | この出力だけ。省略時は出力ごとに1つずつ置かれます |
| `outerEdgesOnly` | `false` | 領域が接する辺の先に別の出力が続く場合、その出力では無効 |
| `leaveMargin` | `0` | 入った後はこの px だけ領域を広げ、そこを出るまで `leave` にしない |
| `pressure` | — | 領域内で画面の端へこの px 押し込んだら `pressure` を通知（滞在中1回） |
| `ignoreWhenFullscreen` | `false` | 全画面ウィンドウを表示中の出力では無効 |
| `ignoreWhileButtonHeld` | `false` | ポインターのボタンを押している間は無効 |

- **配置は出力に追従します。** 出力を基準に指定するので、モニターの抜き差しや
  解像度・スケールの変更があっても定義し直す必要はありません。
- **イベントは順番どおりに届きます。** 1回の移動で別の領域へ移った場合、`leave` が
  すべて先に、`enter` が後に届きます。ある出力の領域から別の出力の同じ領域へ直接
  移ったときも、それぞれの `outputName` を持つ `leave` と `enter` になります。
- **判定はポインターが動いたときに行います。** ポインターが止まったまま全画面に
  なっても、次に動くまでは何も通知されません。
- **定義し直しても状態は保たれます。** 既存の id で `defineRegion` すると、新たな
  `enter` なしで領域が置き換わります。`removeRegion(id)` は `leave` なしで削除します。
- `onPointerRegion` はキーバインドと同じく同期実行で、ハンドラ内のウィンドウ操作は
  すぐに反映されます。短い処理に留めてください。

## 入力グラブ

`COMPOSITOR.input.grab(handlers)` は、返されたグラブを解放するまで入力をすべて
受け取ります。キー・ポインタのボタン・スクロール・スワイプがクライアントではなく
ハンドラーに届きます。デフォルト設定の [Flip 3D](./default-config.md#flip-3d)
スイッチャーのような、コンポジター自身が描くモーダルな UI のためのものです。

```ts
const grab = COMPOSITOR.input.grab({
  onKey(event) {
    // { key: "Escape", keycode, state: "pressed" | "released", modifiers, timestamp }
    if (event.key === "Escape" && event.state === "pressed") grab.release();
  },
  onPointerMotion(event) {},  // { position, delta, outputName, modifiers }
  onPointerButton(event) {},  // { button, buttonName: "left" | ..., state, position, outputName }
  onScroll(event) {},         // { deltaX, deltaY, discreteX, discreteY, source, position }
  onSwipe(event) {},          // GestureSwipeEvent
  onCancel(reason) {},        // "sessionLock" | "error" | "replaced"
});
```

- **すべてグラブに届きます。** 保持中はキーバインドが発火しません。例外はコンポジター
  組み込みのもの（`Super` + `Shift` + `R` / `Q`、`Ctrl` + `Alt` + `F1`〜`F12`）です。
  座標はグローバルな論理ピクセルです。
- **ポインタはカーソルを動かし続けます。** その間クライアントはポインタフォーカスを
  失い（ホバー状態が解除されます）、グラブが終わると戻ります。
- **グラブ開始時に押されていたキー**は、離したイベントがグラブに加えて押下を受け取った
  クライアントにも届きます。グラブを開くために押していた修飾キー（`Super` + `Tab` の
  `Super`）がフォーカス中のウィンドウで押しっぱなしになりません。「押している間だけ
  開く」UI はこの離したイベントで手を離したことを知れます。
- **キー名**はショートカットと同じ xkb の keysym 名です: `"Tab"`・`"Return"`・
  `"space"`・`"Escape"`・`"Left"`・`"Super_L"`・`"a"`。
- **コンポジターは自らグラブを終了します。** 画面ロック時・ハンドラーが例外を投げたとき・
  ホットリロード時です。壊れた設定でデスクトップを操作不能にしないためで、`onCancel` で
  通知されます（リロード時はその設定が消えるので通知はありません）。別のグラブを始めると
  現在のものは置き換えられ、その `onCancel` に `"replaced"` が届きます。
- タッチ入力はグラブしません。

ウィンドウを操作する呼び出し（`window.focus()`、ウィンドウマネージャー経由のアクティブ化）は
キーバインドと同じようにハンドラー内で使えます。3D の配置では
[`pickPlane`](./output-composition-reference.md#当たり判定) でポインタの下の平面を求められます。
