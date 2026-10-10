---
sidebar_position: 5
---

# キーバインドとポインター

## キーボードショートカット

`COMPOSITOR.key.bind(id, shortcut, handler, options?)` はコンポジターレベルの
キーボードショートカットを登録します。

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
| `options` | `{on?: "press" \| "release"}` | 発火タイミング。デフォルトは `"press"` |

### ショートカットの記法

モディファイアとキーを `+` で組み合わせます。

- **モディファイア:** `Super`・`Ctrl`・`Shift`・`Alt`
- **キー:** 英字（`T`・`Q`）、矢印（`Left`・`Right`・`Up`・`Down`）、
  ファンクションキー（`F`）など

```ts
COMPOSITOR.key.bind('close', 'Super+Q', () => focused?.close());
COMPOSITOR.key.bind('move-tile-left', 'Super+Shift+Left', () => moveTile(-1));
COMPOSITOR.key.bind('screenshot', 'Super+P', () => {
  COMPOSITOR.process.spawn({command: 'hyprshot -m region --raw | swappy -f -'});
});
```

### タップバインド（`on: "release"`）

モディファイア単体を `{on: "release"}` で登録すると **タップ** になります。キーを
離したときに発火しますが、その間に他のキーやボタンが押されていない場合に限ります。
デフォルト設定では、`Super` を素早くタップするとランチャーを開きつつ、`Super` を
他のショートカットのモディファイアとしても使えるようにこれを利用しています。

```ts
COMPOSITOR.key.bind('launcher-tap', 'Super', openLauncher, {on: 'release'});
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
ShojiWM は起動するすべてのプロセス（と systemd/D-Bus のアクティベーション環境）に
この変数を渡します。未設定の場合は `WAYLAND_DISPLAY` からソケットを求めます。

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
