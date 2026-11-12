---
sidebar_position: 4
---

# 入力デバイス

`COMPOSITOR.input` はキーボード・ポインター（マウス）・タッチパッドを設定します。
出力と同様に、入力デバイスのセットが変化するたびに呼ばれるファクトリーを登録し、
渡されたドラフトを変更します。

```ts
COMPOSITOR.input.configure((input, context) => {
  // すべての該当デバイスに適用されるグローバルなデフォルト
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

  // デバイス名をキーにしたデバイスごとの上書き
  input.device['Razer Razer Blade Keyboard'] = {
    keyboard: {layout: 'us'},
  };
});
```

`input.global` はその種類の全デバイスに適用されます。`input.device[name]` は特定の
1デバイスの設定を上書きします（`null` を設定すると上書きを解除）。各値は `keyboard`・
`pointer`・`touchpad` のサブオブジェクトを持つ `InputDeviceConfig` です。

## キーボード設定

`keyboard` — `InputDeviceConfig.keyboard` オブジェクト。`rules`／`model`／`layout`／
`variant`／`options` は標準的な XKB 設定です。

| フィールド | 型 | 意味 |
| --- | --- | --- |
| `layout` | `string` | XKB レイアウト（例: `"us"`・`"jp"`・`"de"`） |
| `variant` | `string` | レイアウトのバリアント（例: `"dvorak"`） |
| `options` | `string` | XKB オプション（例: `"caps:ctrl_modifier"`・`"ctrl:nocaps"`） |
| `rules` | `string` | XKB ルールセット |
| `model` | `string` | キーボードモデル |
| `repeatRate` | `number` | 1秒あたりのキーリピート回数 |
| `repeatDelay` | `number` | キーリピート開始までの遅延（ミリ秒） |

```ts
input.global = {
  keyboard: {layout: 'us', options: 'caps:ctrl_modifier', repeatRate: 60, repeatDelay: 250},
};
```

## ノートパソコンの蓋

`COMPOSITOR.event.onLidSwitch` は蓋の開閉を通知します。蓋を閉じている間だけ
内蔵パネルを消す設定は次のとおりです。

```ts
COMPOSITOR.event.onLidSwitch(({closed}) => {
  COMPOSITOR.output.setPower(closed ? 'off' : 'on', {output: 'eDP-1'});
});
```

通知はスケジューラーのティックで届きます。有効化や再読み込みの後の最初のティックで
現在の状態を、その後は変化のたびに通知します。蓋のスイッチが一度も報告していない間は
何も通知しないので、蓋のない機械ではこのイベントは来ません。二つのティックの間に
閉じて開いた場合はまとめられます。登録すると購読解除の関数が返ります。Rust の設定では
`COMPOSITOR.event.on_lid_switch(|lid| ...)` がすぐに呼ばれ、
`COMPOSITOR.input.lid_switch()` が最後の状態を持つシグナルです（最初の報告までは `None`）。

`initial` は、そのイベントが蓋の動きではなく、もともとの状態を伝えているときに `true` です。
スイッチが立ち上がったとき（起動時や別の VT から戻ったとき）に報告した状態や、
リロード後の設定が最初に受け取る状態がこれに当たります。ノートパソコンによっては、
蓋が開いていてもこの形で「閉じている」と報告するので、動作のきっかけには
`initial: false` のイベントだけを使ってください。

このイベントは蓋の状態を知らせるだけです。systemd-logind は `logind.conf` のとおりに
蓋に反応してサスペンドし、その順序は設定とは無関係です。蓋の動作を設定で決めるには、
蓋を logind から引き受けます。[電源と蓋](power.md)を参照してください。

## ポインター（マウス）設定

`pointer` — `InputDeviceConfig.pointer` オブジェクト。

| フィールド | 型 | 意味 |
| --- | --- | --- |
| `pointerAccel` | `number` | 加速の速さ、`-1.0`〜`1.0` |
| `accelProfile` | `"adaptive" \| "flat"` | 加速カーブ（`"flat"` は等倍・加速なし） |
| `naturalScroll` | `boolean` | スクロール方向を反転 |
| `leftHanded` | `boolean` | 左右ボタンを入れ替え |
| `middleEmulation` | `boolean` | 左＋右で中クリックをエミュレート |

```ts
input.global = {pointer: {accelProfile: 'flat', pointerAccel: 0.0}};
```

## タッチパッド設定

`touchpad` — `TouchpadInputConfig`。**上記のポインター設定を継承する**ため、すべての
`pointer` フィールドがここでも有効で、さらに以下が加わります。

| フィールド | 型 | 意味 |
| --- | --- | --- |
| `tapToClick` | `boolean` | パッドをタップしてクリック |
| `tapButtonMap` | `"leftRightMiddle" \| "leftMiddleRight"` | 複数指タップ → ボタンの割り当て |
| `clickMethod` | `"buttonAreas" \| "clickfinger"` | 物理クリックのボタン対応方式 |
| `scrollMethod` | `"none" \| "twoFinger" \| "edge" \| "onButtonDown"` | スクロールの発生方法 |
| `scrollFactor` | `number` | スクロール速度の倍率 |
| `disableWhileTyping` | `boolean` | タイプ中はパッドを無視 |

…に加えて、すべてのポインターフィールド（`pointerAccel`・`accelProfile`・
`naturalScroll`・`leftHanded`・`middleEmulation`）も使えます。

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

## デバイスの読み取りと対象指定

ファクトリーの第2引数（およびコントローラ自身）は接続中のデバイスを公開するため、
種類に応じて条件付きで設定を適用できます。

| メンバー | 返り値 |
| --- | --- |
| `devices` | `InputDeviceInfo[]` |
| `current` | `Record<string, InputDeviceInfo>` |
| `get(key)` | `InputDeviceInfo \| undefined` |
| `find(predicate)` | 最初に一致したデバイス |
| `configure(factory)` | 設定ファクトリーを登録 |
| `reconfigure()` | 全ファクトリーを即時再実行 |

各 `InputDeviceInfo` は `key`・`name`・任意の `vendor`／`product`、そして `kind`
フラグオブジェクト（`keyboard`・`pointer`・`touchpad`・`touch`・`tabletTool`・
`tabletPad`・`gesture`・`switch`）を持ちます。

```ts
COMPOSITOR.input.configure((input, ctx) => {
  for (const device of ctx.devices) {
    if (device.kind.touchpad) {
      input.device[device.key] = {touchpad: {scrollMethod: 'twoFinger'}};
    }
  }
});
```
