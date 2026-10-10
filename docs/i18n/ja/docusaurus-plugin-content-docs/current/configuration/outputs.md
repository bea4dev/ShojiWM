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

### HDR（`hdr`）

出力を HDR10（PQ / BT.2020 の信号と HDR メタデータ）で駆動します。ディスプレイの
EDID が SMPTE ST 2084（PQ）に対応している場合にだけ有効になり、そうでなければ
SDR のままで、理由がログに出ます。実験的機能で、tty でのみ動作します。

| オプション | 意味 |
| --- | --- |
`hdr: true` で既定の設定のまま有効になります。設定を変えるときは代わりに
オブジェクトを渡します（この場合も HDR は有効になります）。

| オプション | 意味 |
| --- | --- |
| `hdr.enabled` | `false` で設定を残したまま SDR で駆動する。既定は `true` |
| `hdr.sdrLuminance` | SDR の白（通常のウィンドウ）の明るさ。cd/m²（10〜1000）か、明るさ設定に連動する `"backlight"`。既定はノート PC では連動、それ以外は 203 |
| `hdr.sdrGamut` | 色の出し方: `"native"`（既定）は SDR モードと同じ、`"srgb"` は正確な色 |
| `hdr.maxLuminance` | EDID に無い場合の実際の最大輝度（cd/m²。無いと 1000 と仮定）。範囲 50〜10000 |
| `hdr.minLuminance` | 実際の黒レベル（cd/m²）。範囲 0〜10 |

```ts
display['eDP-1'] = {
  resolution: 'best',
  position: 'auto',
  hdr: true,
};

display['DP-1'] = {
  hdr: {
    // 外部モニターには連動できるバックライトが無いので、SDR の白を固定する
    sdrLuminance: 250,
  },
};
```

見え方:

- **SDR の内容**（ほぼすべてのウィンドウ）は見た目を保ったまま、白が
  `hdr.sdrLuminance` の明るさで表示されます。
- **ノート PC では明るさキーがそのまま使えます。** HDR の信号は絶対的な明るさを
  指定するため、HDR 中のパネルはバックライト設定を無視します。代わりに ShojiWM が
  明るさ設定を SDR の白の明るさとして反映し、最大にするとパネルの推奨 SDR 最大輝度
  （EDID の値）になります。バックライトの値自体は変更しないので、SDR に戻れば
  そのまま効きます。使う値はドライバの `actual_brightness` で、要求した
  `brightness` と異なることがあります（amdgpu の OLED パネルは曲線で変換します）。
- **SDR の色も SDR モードと同じです。** SDR モードの広色域パネルは、sRGB の値を
  パネル本来の広い色域のまま表示するため、sRGB より鮮やかに見えます。HDR でも既定で
  それに合わせます（`sdrGamut: "native"`、色域はパネルの EDID の値）。`"srgb"` に
  すると正確な色になり、そうしたパネルではくすんで見えます。この設定はすべての
  内容に同じく効きます。Chrome などの色管理対応クライアントは SDR の UI も HDR の
  形式で渡してくるため、例外扱いすると他のウィンドウよりくすんでしまうからです。
  `"native"` では HDR 動画も、パネルが SDR モードで動画を映すときと同じ鮮やかさに
  なります。
- バックライトが無い場合の既定値は放送の基準値 203 cd/m² で、明るいモニターでは
  暗く見えることがあるので、好みで上げてください。
- **HDR の内容**（色管理に対応したクライアント: Chrome / Chromium、mpv、Vulkan の
  ゲーム）は SDR の白より明るく、ディスプレイの最大輝度まで表示され、それを超える
  分はトーンマップされます。クライアントは使える余裕（最大輝度 ÷ SDR の白）を色管理
  プロトコルで知るので、`hdr.sdrLuminance` を上げるほどハイライトの余裕は減ります。
- スクリーンショットと画面録画は SDR のままです。

補足:

- ディスプレイが HDR に対応しているかは `OutputInfo.hdrSupported` で分かります。
- HDMI では、選んだモードで 1 色 10 ビットを伝送できる帯域が必要です。
  `OutputInfo.hdmi` と `availableModes[].clockKhz` で設定から確認できます。
- `SHOJI_HDR_OUTPUTS=eDP-1`（または `all`）で設定なしに HDR を有効にでき、
  `SHOJI_SDR_NITS` でバックライトの無い出力の `hdr.sdrLuminance` の既定値を変えられます。

## パネルの電源を切る（DPMS）

`mode: 'disabled'` は出力をデスクトップから外すので、そこにあったウィンドウは別の
出力へ移動します。離席中に画面を消したいだけなら、代わりに**電源**を切り替えます。
出力はレイアウトに残り、ウィンドウもそのままで、パネルだけが消灯します。TTY
セッションが必要です。ネスト実行にはパネルがないので無視されます。

### アイドルデーモンから

ShojiWM は `wlr-output-power-management` を実装しているので、
[`wlopm`](https://git.sr.ht/~leon_plickat/wlopm) が使えます。swayidle と組み合わせる例:

```sh
swayidle -w \
  timeout 300 'swaylock -f' \
  timeout 600 'wlopm --off "*"' resume 'wlopm --on "*"'
```

### 設定から

```ts
// 次のキー押下・クリック・ポインター移動まで、すべてのパネルを消灯する。
COMPOSITOR.key.bind('screen-off', 'Super+Shift+O', () =>
  COMPOSITOR.output.setPower('off', {wakeOnInput: true}),
);

COMPOSITOR.output.setPower('off', {output: 'HDMI-A-1'}); // 1 つの出力だけ
COMPOSITOR.output.setPower('toggle'); // 対象のどれかが点灯中なら消灯、そうでなければ点灯
COMPOSITOR.output.setPower('on');
```

`setPower` は設定のどこからでも呼べます。キーバインド、タイマー、IPC のハンドラー
（`shoji_wm/ipc` の `createIpcServer`）からも使えるので、自前の IPC メソッドを
用意すれば外部スクリプトからも切り替えられます。Rust の設定では
`COMPOSITOR.output.set_power(OutputPower::Off, OutputPowerOptions { wake_on_input: true, ..Default::default() })`
です。

電源状態のふるまい:

- `wakeOnInput` なしで消した出力は、明示的に点けるまで消えたままです
  （`setPower('on')`、`wlopm --on`）。`wakeOnInput` 付きなら、キー押下・クリック・
  ポインター移動・スクロール・タッチで点灯します。キーを離す操作は数えないので、
  消灯したキーバインド自体ですぐ点灯してしまうことはありません。消灯中に
  `'toggle'` のキーバインドを押すと、点灯するだけです。
- 別の VT に切り替えて戻ると、すべての出力が点灯します。アイドルデーモンが
  落ちていても、消えた画面の前に取り残されることはありません。
- 出力を抜く・無効にすると電源状態は忘れられ、戻ってきたときは点灯しています。
- 消灯中のパネルには何も描画しません。そこにいるクライアントには 1 秒に 1 回
  フレームコールバックを送るので、止まったままにはなりません。その出力の
  スクリーンショットや画面共有は、点灯するまで失敗します（画面共有ポータルは
  再試行します）。

## フレームの出し方（トリプルバッファ）

`COMPOSITOR.rendering.framePacing` は、各出力でフレームをどう出すかを決めます。

| 値 | 動作 |
| --- | --- |
| `"throughput"`（既定） | **先行描画（トリプルバッファ）。** 1 つのフレームが vblank を待っている間に、次のフレームの描画を始めます。 |
| `"low-latency"` | 前のフレームが画面に出てから次のフレームを描画します。 |

```ts
// 全出力（何も設定しない場合の既定と同じ）
COMPOSITOR.rendering.framePacing = "throughput";

// 出力ごと: 出力を受け取る関数。読んだ signal が変わると再評価されます
COMPOSITOR.rendering.framePacing = (output) =>
  output.name.startsWith("DP-") ? "low-latency" : "throughput";
```

### なぜ先行描画するのか

カーネルは 1 つのディスプレイにつき同時に 1 つのページフリップしか受け付けません。
`"low-latency"` では前のフリップが終わるまで次のフレームを描き始められないため、
CPU と GPU の処理を**合わせて**「その vblank から、次の vblank 直前のドライバの
コミット締切まで」に収める必要があります。120Hz なら 8.3ms の周期のうち 5ms 程度です。
ウィンドウが多い、ブラーやシェーダーが重いといったフレームが締切を過ぎると、
1 リフレッシュ遅れて表示され、アニメーションがカクつきます。

`"throughput"` では、前のフレームを提出した時点で次のフレームの描画を始め、
フリップが終わるまで保持します。効果は 2 つあります。

- **1 フレームの持ち時間が増える。** 最大で 1 リフレッシュ分の余裕が増えます。
- **処理が重なる。** あるフレームの GPU 処理と次のフレームの CPU 処理が並行するので、
  1 周期に収める必要があるのは両者の和ではなく大きい方だけになります。

### 代償: 連続描画中だけ 1 フレームの遅延

アニメーション・ドラッグ・動画などでフレームが連続している間は、画面に出るのが
`"low-latency"` より 1 リフレッシュ遅れます。120Hz で約 8ms、60Hz で約 17ms です。

アイドル明けの最初のフレームは何かの先行にはならないので、ターミナルへの
タイピングのような単発の更新は遅れません。

ハードウェアカーソルの鮮度はどちらのモードでも保たれます。ポインターが動いている
間は、先行描画したフレームも vblank 直前まで保持して、カーソルプレーンに最新の位置を
載せます。その分、先行描画で得られる余裕は小さくなります。

### コンポジターが常に低遅延にする場合

`framePacing` の値に関係なく、次の場合は先行描画をしません。

- **テアリング中。** 即時フリップを求めるゲームは、最新のフレームをすぐ出したいためです。
- **フルスクリーンのファストパス（ダイレクトスキャンアウト）。** ゲーム自身のバッファを
  1 フレーム長く握ってしまうためです。
- **カーソルだけの更新。** 静止画面の上でポインターを動かしても再描画は起きません。

環境変数 `SHOJI_RENDER_AHEAD=0` を付けると、すべての出力で先行描画を無効にできます
（比較に便利です）。

実際に適用される設定が変わると（ホットリロード後など）、コンポジターは出力ごとの
結果を `frame pacing changed` としてログに出します。

### モードの選び方

```ts
// バッテリー駆動中は滑らかさ優先、AC 電源では遅延優先
COMPOSITOR.rendering.framePacing = () =>
  onBattery() ? "throughput" : "low-latency";
```

`onBattery` は自分で用意する signal の例です（たとえば `/sys/class/power_supply` を
読む `createPoll` から更新します）。

アニメーションと poll はどちらのモードでも各出力のフレーム時計で進むため、タイミング
は変わりません。変わるのは画面に届くまでの遅延だけです
（[フレームのタイミングと poll](./timing.md) を参照）。

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
| `setPower(power, options?)` | パネルの電源を切り替える（前述） |

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
