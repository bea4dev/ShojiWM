---
sidebar_position: 3
---

# 出力（ディスプレイ）

`COMPOSITOR.output` はモニターのレイアウト――解像度・リフレッシュレート・スケール・
位置・ミラーリング・有効／無効――を制御します。現在の状態を**読む**ことも、希望の
レイアウトを生成する**ファクトリーを登録**することもできます。

## 出力を設定する

`COMPOSITOR.output.configure(factory)` は、**接続中の出力セットが変化するたびに**
（ホットプラグ、ドック接続／取り外しなど）コンポジターが呼ぶ関数を登録します。
ファクトリーは `出力名 → 設定エントリ` のマップを返します。

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

  // ドック接続中はノートPCのパネルを切る
  const docked = context.connected.some((o) => o.name === 'HDMI-A-1');
  if (docked) {
    display['eDP-1'] = {mode: 'disabled'};
  }

  return display;
});
```

出力名（`"DP-1"`・`"eDP-1"`・`"HDMI-A-1"` など）は DRM コネクタ名です。接続中の名前は
`context.connected` または `COMPOSITOR.output.list` を読むと一覧できます。

### 設定エントリ: `mode`

各エントリは `mode` によって3つの形のいずれかになります。

| `mode` | 意味 | 追加フィールド |
| --- | --- | --- |
| `"extend"`（デフォルト） | デスクトップの一部として使う | `resolution` / `position` / `scale` / `transform` / `subpixel` |
| `"disabled"` | 出力をオフにする | — |
| `"mirror"` | 別の出力をミラーする | `source`（ミラー元の出力名）/ `subpixel` |

```ts
display['HDMI-A-1'] = {mode: 'mirror', source: 'eDP-1'};
display['eDP-2'] = {mode: 'disabled'};
```

extend エントリでは `mode` を省略できます（デフォルトのため）。

### `resolution`

DRM モード（サイズ＋リフレッシュレート）を選びます。

| 値 | 意味 |
| --- | --- |
| `"best"` | 出力が提示する最高の解像度＋リフレッシュレート |
| `{width, height}` | そのサイズのモード（一致する中で最高のリフレッシュレート） |
| `{width, height, refreshRate}` | そのモードを正確に指定 |

```ts
display['DP-1'] = {resolution: 'best'};
display['DP-2'] = {resolution: {width: 1920, height: 1080}};
display['DP-3'] = {resolution: {width: 2560, height: 1440, refreshRate: 165}};
```

モニターが対応するモードは `COMPOSITOR.output.availableModes(name)` で確認できます。

### `position`

出力がグローバル座標空間のどこに置かれるかを指定します。

| 値 | 意味 |
| --- | --- |
| `"auto"`（デフォルト） | コンポジターが自動配置（左から右へ） |
| `{x, y}` | 論理ピクセルでの左上隅を明示指定 |

```ts
display['DP-1'] = {position: {x: 0, y: 0}};
display['DP-2'] = {position: {x: 2560, y: 0}}; // DP-1 の右側
```

### `scale`

分数スケール係数（HiDPI）です。`1.0` は等倍、`2.0` は UI を2倍に。デフォルト設定では
`1.5`〜`1.8` のような値を使っています。

```ts
display['eDP-1'] = {resolution: 'best', scale: 1.8};
```

### `transform`

出力の回転・反転です。標準の `wl_output.transform` enum に従います。縦置き
（ポートレート）モニターや上下反転パネルに使います。

| 値 | 意味 |
| --- | --- |
| `"normal"`（デフォルト） | 回転なし |
| `"rotate-90"` / `"rotate-180"` / `"rotate-270"` | 画面を 90°／180°／270° 回転 |
| `"flipped"` | 左右反転 |
| `"flipped-90"` / `"flipped-180"` / `"flipped-270"` | 反転してから回転 |

```ts
// 物理的に縦置きにしたモニター
display['DP-1'] = {
  resolution: 'best',
  position: 'auto',
  transform: 'rotate-90',
};
```

補足:

- `resolution` は常に**物理（回転前）のモード**を指します。縦置きで 1080×1920 に
  したい場合も `{width: 1920, height: 1080}`（または `'best'`）を指定します。
- 設定ドラフトは宣言的です。`transform` を省略（または後から削除）すると
  `"normal"` に戻ります。
- 下流はすべて**回転後の向き**で動きます。`OutputInfo.resolution` は回転後の
  サイズを報告し（そのため `resolution / scale` は常に論理サイズ）、
  `usableArea`・タイリング・スクリーンショット（`grim`）・画面キャプチャ
  （ポータル経由の OBS）も自動的に回転へ追従します。

### `subpixel`

パネルのサブピクセルの物理的な配列です。`wl_output.geometry` でクライアントに
通知されます。foot や Firefox などは、この値を見て文字のアンチエイリアス方法を
決めます。多くのパネルはカーネルに `"unknown"` としか報告しないため、実際の配列を
クライアントに伝える手段はこの設定だけであることがほとんどです。

| 値 | 意味 |
| --- | --- |
| `"unknown"` | 配列が不明（多くのコネクターはこれを報告します） |
| `"none"` | サブピクセル構造なし（プロジェクターなど） |
| `"horizontal-rgb"` / `"horizontal-bgr"` | 横方向に並ぶ（その順序） |
| `"vertical-rgb"` / `"vertical-bgr"` | 縦方向に並ぶ（その順序） |

```ts
// カーネルは "unknown" と報告するが、RGB ストライプだと分かっているノート PC のパネル
display['eDP-1'] = {
  resolution: 'best',
  position: 'auto',
  subpixel: 'horizontal-rgb',
};
```

補足:

- 指定するのは**物理**配列です。コンポジターは `transform` と同じイベントで送り、
  クライアント側が両者を組み合わせるため、回転した出力でも事前に回転させる必要は
  ありません。
- `subpixel` を省略（または後から削除）すると、カーネルがそのコネクターについて
  報告した値に戻ります。その値は `OutputInfo.detectedSubpixel` で常に確認できます。
- 反映は即時です。バインド済みの `wl_output` すべてに新しい `geometry` イベントが
  送られるため、設定のリロードだけで反映され、再接続は不要です。

## 出力の状態を読む

このコントローラは読み取り専用ビューでもあり、イベントハンドラや合成関数の中で
役立ちます。

| メンバー | 返り値 |
| --- | --- |
| `list` | `string[]` — 接続・有効な出力名 |
| `outputs` | `OutputInfo[]` — 全出力のスナップショット |
| `current` | `Record<string, OutputInfo>` — 出力名をキーにしたスナップショット |
| `get(name)` | `OutputInfo \| undefined` |
| `find(predicate)` | 最初に一致した `OutputInfo` |
| `availableModes(name)` | ドライバーが報告する `OutputMode[]` |
| `configure(factory)` | レイアウトファクトリーを登録（前述） |
| `reconfigure()` | 登録済みファクトリーを即時再実行 |

`OutputInfo` には `name`・`enabled`・`resolution`（`{width, height, refreshRate}`）・
`position`（`{x, y}`）・`scale`・`transform`・`subpixel`・`detectedSubpixel`・
`availableModes`、および識別情報（`make`・`model`・`serial`・`connector`）が
含まれます。

`subpixel` は現在通知している配列、`detectedSubpixel` はカーネルが報告した配列です。
設定 UI は、上書きされる前にコネクターが何を報告しているかを表示できます。

transform が設定された出力では、`resolution` は**回転後の向き**で報告されます
（90°／270° では幅と高さが入れ替わる）。一方 `availableModes` は物理のままです。
これにより `resolution / scale` はどの場合でも論理サイズになります。

```ts
const hz = COMPOSITOR.output.get('DP-1')?.resolution?.refreshRate;

// 出力の論理サイズ（解像度をスケールで割る）
const out = COMPOSITOR.output.get('DP-1');
if (out?.resolution) {
  const widthLogical = out.resolution.width / out.scale;
  const heightLogical = out.resolution.height / out.scale;
}
```

:::tip
`COMPOSITOR.output.configure` はハードウェアのレイアウト用です。バーやドックに
重ならないようウィンドウを配置したい場合は、排他ゾーンのレイヤーサーフェスを差し引く
`COMPOSITOR.layer.usableArea(name)` を使ってください。
:::
