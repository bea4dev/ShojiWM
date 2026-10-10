---
sidebar_position: 10
---

# エフェクト

ShojiWM は GPU シェーダーエフェクトを次の場所で実行でき、`COMPOSITOR.effect` で設定します。

| フィールド | 型 | 適用先 |
| --- | --- | --- |
| `background_effect` | `MaybeSignal<CompiledEffectHandle \| null>` | クライアントが要求した領域の背後（`ext-background-effect-v1`） |
| `window` | `(window) => WindowEffectAssignment \| null` | トップレベルウィンドウごと |
| `layer` | `(layer) => LayerEffectAssignment \| null` | レイヤーシェルサーフェスごと（バー・ドック） |
| `popup` | `(popup) => PopupEffectAssignment \| null` | ポップアップごと（メニュー・ツールチップ） |
| [`overlay`](./output-overlays.md) | `(output, options) => Promise<OverlayHandle>` | 一時的または常駐する出力全体のエフェクト |

合成内の領域にエフェクトを適用することもできます
（[`<ShaderEffect/>`](./components.md#shadereffect)）。

## 背景エフェクト

`background_effect` は、クライアントが **`ext-background-effect-v1` Wayland プロトコル**で
要求した領域（サーフェスに宣言された `blur_region`）の背後にコンポジターが描画する
エフェクトです。画面全体の背景では**ありません**。ウィンドウやレイヤーシェルサーフェスが
このプロトコルでオプトインし、コンポジターはその領域の背後にのみこのエフェクトを適用します
――たとえば、背後をぼかすようコンポジターに要求する半透明のアプリやパネルなどです。
`null` で無効化します。

```ts
import {COMPOSITOR, compileEffect, backdropSource, dualKawaseBlur} from 'shoji_wm';

COMPOSITOR.effect.background_effect = compileEffect({
  input: backdropSource(),
  capturePadding: 24,
  invalidate: {kind: 'on-source-damage-box', damagePadding: 8},
  pipeline: [dualKawaseBlur({radius: 4, passes: 2})],
});
```

値には Signal を使え、エフェクト内のシェーダーユニフォームにも Signal を使えます。
変化は次のフレームから反映されます。たとえばエフェクトを切ったり、アニメーションさせたりできます。

```ts
const blur = compileEffect({
  input: backdropSource(),
  alpha: 'preserve',
  pipeline: [
    dualKawaseBlur({radius: 4, passes: 2}),
    shaderStage(loadShader('./src/effect/backdrop-fade.frag'), {
      uniforms: {strength}, // 0〜1 の Signal
    }),
  ],
});
COMPOSITOR.effect.background_effect = computed(() => (paused() ? null : blur));
```

## ウィンドウ／レイヤー／ポップアップごとのエフェクト

各ファクトリーはサーフェスごとに呼ばれ、割り当てを返すか、エフェクト無しなら
`null`／`{}` を返します。レイヤーとポップアップの割り当ては `behind` を使って
サーフェスの背後にエフェクトを描画します（デフォルト設定はバーやメニューの背後を
すべてぼかします）。

```ts
const LAYER_BLUR = compileLayerEffect({
  input: backdropSource(),
  capturePadding: 24,
  invalidate: {kind: 'on-source-damage-box', damagePadding: 8},
  alpha: 'preserve',
  pipeline: [dualKawaseBlur({radius: 4, passes: 2})],
});

COMPOSITOR.effect.layer = (layer) => {
  if (layer.namespace() === 'no_blur') return {};
  return {behind: LAYER_BLUR};
};

COMPOSITOR.effect.popup = (popup) => {
  if (popup.parentKind === 'window') return {};
  return {behind: POPUP_BLUR};
};
```

### レイヤーエフェクトを領域に絞る

レイヤーの `behind` バックドロップは、既定ではレイヤーサーフェス全体を覆います。
背後のキャプチャもパイプラインもサーフェス全体で行われ、その下のどこかが damage
されるたびにエフェクトが再実行されます。シェル系のツールキットは、描画内容より
ずっと大きなサーフェスを使うことがよくあります（アニメーションするピルを囲む
固定サイズのサーフェスや、入力マスクでバーだけ切り出したモニターごとの全画面
サーフェスなど）。その場合は重くなるので、`region` で描画している部分だけに
絞ってください。

```ts
const BAR_BLUR = compileLayerEffect({
  input: backdropSource(),
  region: 'input', // 入力領域（QuickShell の `mask` など）
  outsets: 16,     // 領域からはみ出して描く分（アンチエイリアス、形の融合）
  alpha: 'preserve',
  pipeline: [dualKawaseBlur({radius: 4, passes: 2})],
});
```

| `region` | 覆う範囲 |
|---|---|
| `'surface'`（既定） | サーフェス全体。 |
| `'input'` | 入力領域（`wl_surface.set_input_region`）の外接矩形。未設定のサーフェスは全体で入力を受けるので、その場合はサーフェス全体になります。 |
| `'blur-region'` | ext-background-effect でクライアントが要求したブラー領域（QuickShell の `BackgroundEffect.blurRegion`）の外接矩形。要求が無い間は何も描きません。 |

キャプチャ・パイプライン・`layerSource()`・damage による無効化のすべてが、この
領域（＋`outsets`）に従います。どちらの領域もバッファと同じ commit で切り替わるため、
アニメーションする形に対してエフェクトが 1 フレーム遅れることはありません。
領域の外に描かれたものにはバックドロップが掛かりません。`'surface'` 以外を指定
できるのはバックドロップの `behind` だけです。

## 特定のウィンドウやレイヤーだけをぼかす

半透明のターミナルなど一部のアプリだけ、一部のバーだけ背後をぼかしたい、というのは
よくある構成です。どちらも、そのサーフェスに何を割り当てるかを決める関数の中で、
サーフェスの識別子を読むだけで実現できます。

### サーフェスを見分ける

| サーフェス | 識別子 | 読む場所 |
| --- | --- | --- |
| ウィンドウ | `window.appId()` — Wayland の `app_id`。Xwayland アプリでは X11 の `WM_CLASS` のクラス（例: `"kitty"`、`"org.gnome.Nautilus"`、`"steam"`） | `COMPOSITOR.window.composition` |
| ウィンドウ | `window.title()` | `COMPOSITOR.window.composition` |
| レイヤーサーフェス | `layer.namespace()` — クライアントが決める名前（例: waybar の `"waybar"`、QuickShell の `WlrLayershell.namespace`） | `COMPOSITOR.effect.layer` |

どちらもシグナルなので、値が変わると判定がやり直されます（マップ後に `app_id`
を設定するブラウザでも正しいエフェクトになります）。使っているアプリの値を調べるには、
一度ログに出して `~/shoji_wm/logs/latest.log` を見てください。

```ts
COMPOSITOR.effect.layer = (layer) => {
  console.info('layer namespace:', layer.namespace());
  return {};
};
```

### ウィンドウ: 装飾を `<ShaderEffect/>` で包む

ウィンドウの背景ブラーは装飾の一部です。対象のアプリでは、タイトルバーと
`<ClientWindow/>` を [`<ShaderEffect/>`](./components.md#shadereffect) の中に置き、
それ以外は普通の `<Box/>` にします。エフェクトは装飾された領域全体を覆い、
外側の [`<WindowBorder/>`](./components.md#windowborder) の角丸に沿います。

```tsx
import {
  COMPOSITOR, compileEffect, backdropSource, dualKawaseBlur,
  ManagedWindow, WindowBorder, ShaderEffect, Box, Label, ClientWindow,
} from 'shoji_wm';

// コンポジション関数の外で一度だけコンパイルする
const FROSTED = compileEffect({
  input: backdropSource(),
  capturePadding: 24,
  invalidate: {kind: 'on-source-damage-box', damagePadding: 8},
  alpha: 'preserve',
  pipeline: [dualKawaseBlur({radius: 4, passes: 2})],
});

const BLURRED_APPS = ['kitty', 'ghostty', 'org.gnome.Nautilus'];

COMPOSITOR.window.composition = (window) => {
  const content = [
    <Box direction="row" style={{height: 28, paddingX: 8, alignItems: 'center'}}>
      <Label text={window.title} style={{flexGrow: 1, fontSize: 13}} />
    </Box>,
    <ClientWindow />,
  ];
  const blurred = BLURRED_APPS.includes(window.appId() ?? '');

  return (
    <ManagedWindow rect={window.position} zIndex={1}>
      <WindowBorder style={{borderRadius: 10, border: {px: 2, color: '#4f5666'}}}>
        {blurred ? (
          <ShaderEffect shader={FROSTED} direction="column">{content}</ShaderEffect>
        ) : (
          <Box direction="column">{content}</Box>
        )}
      </WindowBorder>
    </ManagedWindow>
  );
};
```

デフォルト設定は kitty と ghostty に対してまさにこれを行っています
（`packages/config/src/index.tsx` の `TERMINALS` を検索）。ただし単純なブラーではなく、
次に説明する liquid glass シェーダーを使っています。

:::note アプリ側を半透明にする必要があります
ブラーはクライアントの**背後**に描かれるので、クライアント自身のピクセルが透明な
部分からしか見えません。kitty なら `background_opacity 0.8`、ghostty なら
`background-opacity = 0.8` のように、アプリ側の透過を有効にしてください。
不透明なウィンドウは見た目が変わらず、（半透明なら）タイトルバーにだけ効果が出ます。
:::

### liquid glass を強くする

デフォルト設定の liquid glass ステージは、背景をわずかにしか曲げません。見た目は
uniform で調整できます。

| uniform | 効果 |
| --- | --- |
| `distortion_depth` | 歪みが縁から内側へどこまで及ぶか（ウィンドウの短辺に対する割合）。大きいほどレンズの縁が太くなります。 |
| `distortion_strength` | 縁で背景をどれだけ引き寄せるか。「どれだけガラスらしいか」の主なつまみです。 |
| `chromatic_shift_px` | 縁の色ずれ（ピクセル）。`0` で無効。 |
| `glass_tint` | 背景の明るさの倍率。`1` 未満で暗くなり、文字が読みやすくなります。 |
| `glass_radius_px` | レンズの角の半径。`-1` でウィンドウの角丸に追従します。 |

```ts
shaderStage(loadShader('./src/effect/liquid-glass.frag'), {
  uniforms: {
    glass_radius_px: -1.0,
    distortion_depth: 0.35,    // デフォルト設定: 0.2
    distortion_strength: 0.4,  // デフォルト設定: 0.15
    chromatic_shift_px: 4.0,
    glass_tint: 0.85,
  },
}),
```

### レイヤー: namespace で選ぶ

バー・ドック・ランチャーはレイヤーサーフェスなので、代わりに
`COMPOSITOR.effect.layer` で扱います。ぼかしたい namespace には `{behind: ...}` を、
それ以外には `{}` を返します。

```ts
const PANEL_BLUR = compileLayerEffect({
  input: backdropSource(),
  capturePadding: 24,
  invalidate: {kind: 'on-source-damage-box', damagePadding: 8},
  alpha: 'preserve',
  pipeline: [dualKawaseBlur({radius: 4, passes: 2})],
});

const BLURRED_LAYERS = ['waybar', 'rofi'];

COMPOSITOR.effect.layer = (layer) =>
  BLURRED_LAYERS.includes(layer.namespace() ?? '') ? {behind: PANEL_BLUR} : {};
```

デフォルト設定は逆に、すべてのレイヤーをぼかし、クライアントが `no_blur` という
namespace で除外できるようにしています。さらに `layerSource()` でブラーをレイヤー自身の
不透明ピクセルに切り抜いているので、角丸や浮いたパーツのあるバーでも透明な隙間は
ぼかされません（デフォルト設定の `LAYER_BLUR_MASK`）。

## エフェクトを組み立てる

エフェクトは **ソース入力＋ステージのパイプライン**です。使う場所に応じたコンパイル
関数でコンパイルします。

| コンパイラ | 生成物 | 用途 |
| --- | --- | --- |
| `compileEffect(opts)` | `CompiledEffectHandle` | 背景・`<ShaderEffect/>` |
| `compileWindowEffect(opts)` | `WindowEffectHandle` | `COMPOSITOR.effect.window` |
| `compileLayerEffect(opts)` | `LayerEffectHandle` | `COMPOSITOR.effect.layer` |
| `compilePopupEffect(opts)` | `PopupEffectHandle` | `COMPOSITOR.effect.popup` |

オプション:

| オプション | 型 | 意味 |
| --- | --- | --- |
| `input` | ソースハンドル | パイプラインが読む対象（例: `backdropSource()`） |
| `capturePadding` | `MaybeSignal<number>` | 可視内容の周囲に追加で取り込み、全ステージで処理する論理ピクセル数（デフォルト `0`） |
| `pipeline` | ステージ配列 | 順に適用されるステージ |
| `invalidate` | ポリシー | 再描画のタイミング（デフォルト: paddingなしのsource damage） |
| `alpha` | `"opaque" \| "preserve"` | 最終出力のアルファ方針（デフォルト `"opaque"`） |
| `outsets` | `EffectOutsets` | （ウィンドウ／レイヤー／ポップアップエフェクト）サーフェス境界の外側に描画 |

### ソース

| ソース | 読み取る対象 |
| --- | --- |
| `backdropSource()` | 対象の背後に合成済みのシーン |
| `xrayBackdropSource()` | 現在のサーフェスが存在しないものとして取得した背景 |
| `windowSource()` | ウィンドウ自身のサーフェス |
| `layerSource()` | レイヤーサーフェス自身の内容 |
| `popupSource()` | ポップアップ自身の内容 |
| `imageSource(path)` | 静的な画像ファイル |
| `shaderInput(shader, opts)` | シェーダーが生成する入力テクスチャ |
| `get(name)` | `save(name)`で先に保存した中間結果 |
| `stateSource(state)` | 永続状態テクスチャの最新値 |

`windowSource`、`layerSource`、`popupSource`には
`{include: 'full' | 'root-surface'}`を指定できます。デフォルトは`'full'`です。

TTY バックエンドでは、full のウィンドウソースはルートサーフェス、サブサーフェス、
サーバーサイドデコレーションを含みます。full ソースの置き換えはその画像全体に適用され、
サブサーフェスがその上に二重に描かれることはありません。ポップアップは引き続き独立して
合成されます。root-surface ソースは従来どおりサブサーフェスを含みません。

### サブサーフェス

サブサーフェス（埋め込み動画、プレビュー、一部のブラウザの内容など）はウィンドウの外へ
はみ出すことがあり、その部分は full のウィンドウソースではカバーできません。ウィンドウ
専用の 2 つのスロットは、サブサーフェスを本体とは別に、サブサーフェス自身の範囲で
扱います。`replaceSubsurfaces` は置き換え、`behindSubsurfaces` はその背後に（`outsets`
も含めて）描画します（影など）。

```ts
COMPOSITOR.effect.window = () => ({
  replace: DISSOLVE,
  replaceSubsurfaces: DISSOLVE,
  behind: SHADOW,
  behindSubsurfaces: SHADOW,
});
```

- これらのスロットの `windowSource()` はサブサーフェスを読みます。`behindSubsurfaces`
  は `replaceSubsurfaces` 適用前のサブサーフェスを読みます。どちらかを設定している間は、
  他のすべてのスロットのウィンドウソースからサブサーフェスが除かれます（ルート
  サーフェスとサーバーサイドデコレーションのみ）。
- `behindSubsurfaces` はサブサーフェスの直後ろに描かれるため、ウィンドウ内のサブ
  サーフェスではウィンドウの内容の上に重なります。
- ルートサーフェスより上と下のサブサーフェスは、重なり順を保つため 2 つのグループとして
  処理されます。`replace` が無いとき下側のグループはクライアントとデコレーションの間に、
  `replace` があるときはその置き換え結果の下に描かれます。
- テクスチャはウィンドウではなくサブサーフェスを覆います。マスクをウィンドウに揃えるには
  `effect_frame_uv(effect)`（[`shader_main` の約束ごと](#shader_main-の約束ごと)を参照）で計算
  してください。同じシェーダーを使えば、両スロットにまたがって 1 枚につながったマスクに
  なります（ウィンドウ用とサブサーフェス用のスロットの間で共通）。
- エフェクトが失敗したときは、サブサーフェスをそのまま描画します。
- TTY バックエンドのみ。閉じるアニメーションはクライアント領域の凍結スナップショットから
  描画されるため、これらのスロットは使われません。

### ステージ

| ステージ | 目的 |
| --- | --- |
| `dualKawaseBlur({radius, passes})` | 高速で広いブラー |
| `shaderStage(shader, {uniforms, textures})` | カスタム GLSL フラグメントシェーダーを実行 |
| `noise({...})` | フィルムグレイン風のノイズを追加 |
| `save(name)` / `blend(input, {...})` | 中間結果の保存／合成 |
| `unit(effect)` | コンパイル済みの再利用可能なサブエフェクトを埋め込む |
| `renderTo(state, {...})` | 永続状態へサイドパイプラインを実行 |
| `renderToIfDirty(state, {dependsOn, ...})` | `renderTo` と同じだが、宣言したソースが変化したときだけ再実行 |

`dualKawaseBlur`の`radius`はデフォルト`8`でサンプリング間隔を制御し、`passes`は
デフォルト`2`でダウン／アップサンプルの深さを制御します（`0`〜`8`に制限）。どちらを
変更しても`capturePadding`や`damagePadding`は自動変更されません。

`shaderStage` はシェーダー（パス、または `loadShader(path)` ハンドル）に加えて、
`uniforms`（シェーダーに渡す数値・色）と `textures`（名前で束縛する追加のソース
ハンドル）を取ります。

uniform値には数値または2／3／4成分の配列を指定でき、各成分をsignalにできます。
`tex`、`effect_texture_size_phy_px`、`effect_content_rect_phy_px`、`effect_frame_rect_phy_px`（と旧名の`_px`版）は
コンポジターが使用する予約bindingなので、独自のuniform名やtexture名には使えません。

```ts
import {compileEffect, backdropSource, dualKawaseBlur, shaderStage, loadShader} from 'shoji_wm';

const liquidGlass = compileEffect({
  input: backdropSource(),
  capturePadding: 24,
  invalidate: {kind: 'on-source-damage-box', damagePadding: 8},
  pipeline: [
    dualKawaseBlur({radius: 4, passes: 2}),
    shaderStage(loadShader('./src/liquid-glass.frag'), {
      uniforms: {
        glass_radius_px: 10.0,
        distortion_strength: 0.15,
        chromatic_shift_px: 3.0,
      },
    }),
  ],
});
```

### 永続状態テクスチャ

`save()`と`get()`は同じフレーム内で使う中間保存です。シミュレーション、残像、蓄積
マスクなど、フレームを跨いでGPU上の値を保持するエフェクトには`stateTexture()`を
宣言し、`renderTo()`で更新します。

```ts
const velocity = stateTexture('velocity', {
  scale: 0.5,
  format: 'rg16f',
  resize: 'clear',
});
const dye = stateTexture('dye', {
  scale: 0.5,
  format: 'rgba16f',
  resize: 'stretch',
});

const fluid = compileLayerEffect({
  input: layerSource(),
  invalidate: {kind: 'manual', dirtyWhen: fluidActive},
  pipeline: [
    renderTo(velocity, {
      input: stateSource(velocity),
      pipeline: [
        shaderStage(loadShader('./velocity.frag'), {
          textures: {dye: stateSource(dye)},
          uniforms: {dt: frameDelta},
        }),
      ],
    }),
    renderTo(dye, {
      input: stateSource(dye),
      pipeline: [
        shaderStage(loadShader('./dye.frag'), {
          textures: {velocity: stateSource(velocity)},
        }),
      ],
    }),
    shaderStage(loadShader('./fluid-compose.frag'), {
      textures: {dye: stateSource(dye)},
    }),
  ],
});
```

同じハンドルを共有しても、各ウィンドウ／レイヤー／ポップアップのエフェクトインスタンスは
別々の状態を所有します。`renderTo()`は外側のパイプラインの現在テクスチャを置き換えません。
`stateSource()`は最初の書き込み前には前フレームの値を、書き込み後にはその時点の最新値を
読みます。同じ状態の読み書きも、内部でping-pongテクスチャを使うため安全です。

`scale`はエフェクト処理解像度に対する倍率です。形式は`rgba8`、`rg16f`、`rgba16f`で、
現在の`rg16f`は内部的にはRGBA16Fを確保し、R/Gチャンネルを状態として使います。リサイズ時は
`clear`なら透明黒から再開し、`stretch`なら直前の状態を新しい大きさへ引き伸ばします。
互換性のあるホットリロードでは状態を維持し、所有するエフェクトインスタンスのキャッシュが
破棄または追い出されたときに解放します。

#### 変化のないサイドパイプラインを飛ばす：`renderToIfDirty()`

`renderTo()`は外側のパイプラインが走るたびに毎回実行されます。時間方向に蓄積する
エフェクトにはこれが必要です。一方で、レイヤーのシルエットから作る距離場やマスクのように
「重いが対象そのものにしか依存しない」サイドパイプラインもあります。外側のパイプラインは
背景が変わるたびに再実行されるので、そのたびに作り直すのは無駄です。
`renderToIfDirty()`は、`dependsOn`に挙げたソースが前回の書き込みから変化したときだけ
サイドパイプラインを再実行し、それ以外では丸ごとスキップします。

```ts
const field = stateTexture('silhouette-field', {format: 'rgba16f'});

const glass = compileLayerEffect({
  input: backdropSource(),
  invalidate: {kind: 'on-source-damage-box', damagePadding: 64},
  pipeline: [
    dualKawaseBlur({radius: 2, passes: 2}),
    renderToIfDirty(field, {
      dependsOn: [layerSource()],
      input: layerSource(),
      pipeline: [shaderStage(loadShader('./src/field.frag'))],
    }),
    shaderStage(loadShader('./src/glass.frag'), {
      textures: {field: stateSource(field)},
    }),
  ],
});
```

- `dependsOn`には`windowSource()`・`layerSource()`・`popupSource()`を指定できます。
  そのソースとしてキャプチャされた内容（新しいコミット、キャプチャ内でのサイズや位置）が
  変わると「変化した」と判定されます。
- **サイドパイプラインが実際に読むソースはすべて宣言してください。** コンポジターは
  GLSLの中身を見られないため、宣言漏れがあると古い結果が残ります。`shaderStage`は
  前段の出力を必ず`tex`として受け取る点に注意してください。シェーダーがそれを読むなら、
  その前段を作ったものも依存先です。`SHOJI_RENDER_TO_IF_DIRTY_ALWAYS=1`を付けて
  コンポジターを起動すると、すべての`renderToIfDirty()`が`renderTo()`と同じ動作になり、
  表示の乱れが宣言漏れによるものか切り分けられます。
- **結果は`stateSource(state)`で読みます。** サイドパイプライン内で`save()`した名前は
  スキップされた実行では存在しないため、サイドパイプラインの外から`get()`すると
  設定エラーになります。内側で使うのは問題ありません。
- 状態にはサイドパイプラインの最終テクスチャがそのまま入ります。`renderTo()`が行う
  コピーを経ないので、サイドパイプライン内の`save()`/`get()`と同じ座標系です。
- 状態が再確保されたとき（初回・リサイズ・フォーマット変更）と、サイドパイプライン自身の
  ユニフォームが変化したときは、依存先に関係なく再実行されます。

### 無効化ポリシー

`invalidate` はエフェクトの再描画タイミングを制御し、新鮮さとコストのバランスを取ります。

- `{kind: 'on-source-damage-box', damagePadding: N}` — 変化した領域だけを再描画し、
  ダメージの発火判定と再描画領域の両方を `N` 論理ピクセル広げます。通常はこれを選びます。
- `{kind: 'always'}` — 毎フレーム再描画（高コスト。アニメーションするシェーダー向け）。
- `{kind: 'manual', dirtyWhen, base?}` — `MaybeSignal<boolean>`の`dirtyWhen`がtrueの間は
  再描画します。falseの間は、任意の自動`base`ポリシーが無効化しない限りキャッシュを
  再利用します。

手動制御しながら周囲のsource damageにも反応させる例です。

```ts
invalidate: {
  kind: 'manual',
  dirtyWhen: effectParametersChanged,
  base: {kind: 'on-source-damage-box', damagePadding: 24},
}
```

`damagePadding` はシェーダー入力の大きさを広げません。ブラーや歪みなどが可視領域の
外側のピクセルを参照する必要がある場合は `capturePadding` を使います。

### キャプチャパディング

`capturePadding: N` を指定すると、ShojiWM はエフェクトの可視内容の周囲を `N` 論理
ピクセル余分に取り込みます。このパディング付きテクスチャは、カスタムシェーダー、
ブラー、保存した中間結果、ブレンドを含むパイプライン全体を通ります。可視領域への
切り落としは、最後のステージが終わった後に一度だけ行われます。

これにより、周辺ピクセルを参照するエフェクトで端のクランプを避けられます。値は論理
ピクセルで指定し、物理テクスチャの確保時には ShojiWM が出力スケールを適用します。
一方、GLSLへ渡される`EffectContext`のピクセル値は物理ピクセルです。`0`の場合は可視
領域と同じ大きさです。

ShojiWMはカスタムシェーダーのサンプリング範囲を自動推論できません。もっとも遠い参照を
覆える`capturePadding`を指定し、通常は同じ影響範囲を`damagePadding`でも覆ってください。
テクスチャ端のクランプは安全のための挙動にすぎず、切り取られた端のピクセルを繰り返す
だけなので、取り込まなかった周囲のシーンは復元できません。物理的な画面端では、存在
しないパディングが自然に切り詰められます。

### アルファ

デフォルトの`alpha: 'opaque'`では、最終パスがアルファを`1.0`に強制します。これは、
キャプチャ端のアルファを有効な内容として扱わない通常の背景ブラーに適しています。
レイヤー自身のアルファマスクで切り抜くブラーなど、パイプラインが意図的に透明度を
生成する場合は`alpha: 'preserve'`を指定します。このモードでは作業テクスチャ全体で
意味のあるアルファを生成する責任がシェーダーパイプラインにあります。

---

## カスタムシェーダーを書く

組み込みステージだけでは足りないときは、自分で**フラグメントシェーダー**を書いて
`shaderStage` で実行できます。フラグメントシェーダーとは、領域の**ピクセルごとに1回**
GPU が実行する小さなプログラムで、そのピクセルの最終的な色を計算するのが役割です。
ShojiWM のシェーダーは **GLSL ES 1.00**（WebGL 1 / OpenGL ES 2.0 と同じ方言）で書き、
設定ファイルの隣の `.frag` ファイルに置きます。

:::tip[シェーダーは初めてですか？]
シェーダーを一度も書いたことがないと、「ピクセルごとに考える」という発想に慣れるまで
少し時間がかかります。次の入門サイトはどれもとても分かりやすいので、まず1つ読んでから
戻ってくると理解がスムーズです。

- [The Book of Shaders](https://thebookofshaders.com/) — もっとも易しい入り口
- [Shadertoy](https://www.shadertoy.com/) — ブラウザでライブに実験できる
- [Khronos GLSL ES クイックリファレンス](https://www.khronos.org/files/opengles_shading_language.pdf) — 組み込み関数一覧

ShojiWM 用に書く GLSL もまったく同じ言語です。違うのはエントリーポイントと、
あらかじめ用意されたいくつかの変数（後述）だけです。
:::

### `shader_main` の約束ごと

完全な GLSL プログラムを書くのではなく、**1つの関数**だけを書きます。

:::warning[シェーダー ABI]
現在の ABI では `shader_main(EffectContext)` が必要です。以前の
`shader_main(vec2 uv, vec2 rect_size)` を使うシェーダーは移行してください。
:::

```glsl
vec4 shader_main(EffectContext effect) {
    // このピクセルの色を計算して返す
    return vec4(1.0, 0.0, 0.0, 1.0); // 不透明な赤
}
```

ShojiWM は `EffectContext` を定義し、値を構築してから関数を呼びます。重要な契約は
次と同等です。

```glsl
struct EffectContext {
    vec2 texture_uv;           // 作業テクスチャ全体の正規化座標
    vec2 texture_size_phy_px;  // 作業テクスチャ全体の物理ピクセルサイズ
    vec4 content_rect_phy_px;  // 可視内容の x, y, width, height
    vec4 frame_rect_phy_px;    // エフェクトが属するウィンドウの矩形
    vec4 frame_radius_phy_px;  // その角丸: 左上・右上・右下・左下
    vec4 frame_border_phy_px;  // その枠幅: 上・右・下・左
    vec4 clip_rect_phy_px;     // ShojiWM がエフェクトに掛けるクリップ（テクスチャ内）
    vec4 clip_radius_phy_px;
    bool has_clip;
    float scale;               // 論理1pxあたりの物理ピクセル数
};
```

このおかげで、`shader_main` の中では次の組み込みが最初から使えます。

| 名前 | 型 | 意味 |
| --- | --- | --- |
| `effect.texture_uv` | `vec2` | キャプチャパディングを含むテクスチャ全体の正規化座標 |
| `effect.texture_size_phy_px` | `vec2` | 作業テクスチャ全体の物理ピクセルサイズ |
| `effect.content_rect_phy_px` | `vec4` | テクスチャ内の可視内容を `(x, y, width, height)` で表した矩形 |
| `effect.frame_rect_phy_px` | `vec4` | ウィンドウエフェクトでは、テクスチャ内のウィンドウ自身の矩形（`outsets` を含まない）。テクスチャが別の範囲を覆う場合（`replaceSubsurfaces`）も同じ。それ以外では可視内容の矩形 |
| `effect.frame_radius_phy_px` | `vec4` | フレームの角丸（左上・右上・右下・左下） |
| `effect.frame_border_phy_px` | `vec4` | フレームの枠幅（上・右・下・左） |
| `effect.clip_rect_phy_px` | `vec4` | ShojiWM がエフェクトに掛ける角丸クリップのテクスチャ内矩形 `(x, y, width, height)`。`has_clip` のときのみ有効 |
| `effect.clip_radius_phy_px` | `vec4` | そのクリップの角丸 |
| `effect.has_clip` | `bool` | エフェクトがクリップされているか（ウィンドウの角丸など） |
| `effect.scale` | `float` | 論理1pxあたりの物理ピクセル数。論理pxの長さに掛ければ、どのスケールでも同じ見た目になります |
| `tex` | `sampler2D` | このステージの入力。`texture2D(tex, effect.texture_uv)` でサンプリング |

フレームが何を指すかは、エフェクトの置き場所によって変わります。

| 置き場所 | フレーム（矩形・角丸・枠） | クリップ |
| --- | --- | --- |
| `<ShaderEffect/>` | コンポーネント自身（`borderRadius`・枠） | 角丸の祖先（ウィンドウの角など） |
| ウィンドウのスロット | ウィンドウ（`<WindowBorder>`、無ければルート）。`windowSource({include: 'root-surface'})` では枠の内側のクライアント領域 | なし |
| レイヤー・ポップアップ・背景 | 覆う範囲。角丸と枠は 0（形はクライアントが自分で描くため） | なし |

すべての`*_phy_px`値は物理ピクセル（論理ピクセル×出力スケール）です。さらに次のヘルパーを利用できます。

| ヘルパー | 結果 |
| --- | --- |
| `effect_texture_phy_px(effect)` | 作業テクスチャ全体における現在のフラグメント位置 |
| `effect_content_phy_px(effect)` | 可視内容の左上を原点とした現在のフラグメント位置 |
| `effect_content_uv(effect)` | 可視内容上で`0.0`〜`1.0`、パディング部分では範囲外となる正規化座標 |
| `effect_texture_uv_from_content_phy_px(effect, px)` | 可視内容基準の物理ピクセルをサンプリング用texture UVへ変換 |
| `effect_frame_phy_px(effect)` | フレームの左上を原点とした現在のフラグメント位置 |
| `effect_frame_uv(effect)` | フレーム基準の正規化座標。ウィンドウ上で`0.0`〜`1.0`、その外では範囲外 |
| `effect_frame_sdf(effect)` | フレームの角丸矩形までの符号付き距離（内側が負） |
| `effect_frame_coverage(effect)` | フレームの角丸矩形のカバレッジ（物理1px幅のアンチエイリアス） |
| `effect_frame_inner_sdf(effect)` | フレームの枠の内側の縁までの符号付き距離 |
| `effect_clip_sdf(effect)` / `effect_clip_coverage(effect)` | クリップについて同じもの（クリップが無ければ常に内側） |
| `shoji_rrect_sdf(p, rect, radius)` / `shoji_coverage(sdf)` | 汎用の角丸矩形の距離とカバレッジ（[ペイントシェーダー](./paint.md#ヘルパー)と同じ） |

例えば、ウィンドウの角に沿った縁のハイライトは次のように書けます。

```glsl
vec4 shader_main(EffectContext effect) {
    vec4 color = texture2D(tex, effect.texture_uv);
    float d = effect.has_clip ? effect_clip_sdf(effect) : effect_frame_sdf(effect);
    float rim = 1.0 - smoothstep(0.0, 1.5 * effect.scale, abs(d + 0.75 * effect.scale));
    return vec4(color.rgb + rim * 0.25, color.a);
}
```

:::note[旧名]
`_phy_px` が付く前は `texture_size_px`・`content_rect_px`・`frame_rect_px`・
`effect_texture_px`・`effect_content_px`・`effect_texture_uv_from_content_px`・
`effect_frame_px` という名前でした。旧名も同じ値のまま使えますが、新しいシェーダーでは
`_phy_px` の名前を使ってください。
:::

サンプリングにはtexture UV、可視矩形に結び付く形状計算にはcontent座標を使ってください。
作業テクスチャではキャプチャパディングが可視内容より前に置かれるため、content rectの
開始位置は0以外になることがあります。

ピクセルの色は、各成分が `0.0`〜`1.0` の `vec4(r, g, b, a)` で返します。

物理ピクセル単位の歪みは、移動後のcontent位置をtexture UVへ戻してサンプリングします。
先にcontent UVを`0.0`〜`1.0`へクランプすると`capturePadding`が供給したピクセルを
捨ててしまうため、そうしないでください。

```glsl
uniform vec2 displacement_px;

vec4 shader_main(EffectContext effect) {
    vec2 sample_px = effect_content_phy_px(effect) + displacement_px;
    vec2 sample_uv = clamp(
        effect_texture_uv_from_content_phy_px(effect, sample_px),
        vec2(0.0),
        vec2(1.0)
    );
    return texture2D(tex, sample_uv);
}
```

### 最初のシェーダー：単色

もっとも単純なシェーダーは、すべてを無視して1色を返します。

```glsl
// shaders/white.frag
vec4 shader_main(EffectContext effect) {
    return vec4(1.0, 1.0, 1.0, 1.0); // 不透明な白
}
```

`loadShader` ＋ `shaderStage` でエフェクトに組み込み、それをエフェクトが使える場所
（ここでは [`<ShaderEffect/>`](./components.md#shadereffect)）で利用します。

```tsx
import {compileEffect, backdropSource, shaderStage, loadShader} from 'shoji_wm';

const white = compileEffect({
  input: backdropSource(),
  pipeline: [shaderStage(loadShader('./shaders/white.frag'))],
});

<ShaderEffect shader={white} style={{width: 100, height: 40}} />
```

### ソーステクスチャを読む

たいていは単色を塗るのではなく、領域の*背後にあるもの*を加工したいはずです。ソースは
`texture2D(tex, effect.texture_uv)` でサンプリングします。次のシェーダーは背景をグレースケールに
脱色します。

```glsl
// shaders/grayscale.frag
vec4 shader_main(EffectContext effect) {
    vec4 source = texture2D(tex, effect.texture_uv);
    float gray = dot(source.rgb, vec3(0.299, 0.587, 0.114));
    return vec4(vec3(gray), source.a);
}
```

`texture2D(tex, effect.texture_uv)` は、いま処理しているピクセル位置のソースの色を `vec4`（RGBA）で
返します。あとは普通の計算です。

### パラメータ：ユニフォーム

シェーダーを設定可能にするには、`uniform` 変数を宣言し、その値を `shaderStage` から
渡します。ユニフォームは1回の描画の全ピクセルで同じ値です。

```glsl
// shaders/tint.frag
uniform vec3 tint;       // RGB の色
uniform float strength;  // 0..1

vec4 shader_main(EffectContext effect) {
    vec4 source = texture2D(tex, effect.texture_uv);
    vec3 tinted = mix(source.rgb, tint, strength);
    return vec4(tinted, source.a);
}
```

```ts
shaderStage(loadShader('./shaders/tint.frag'), {
  uniforms: {
    tint: [0.84, 0.73, 0.49], // vec3
    strength: 0.3,            // float
  },
});
```

TypeScript 側の値の型は、長さによって GLSL のユニフォーム型に対応します。

| `uniforms` の値 | GLSL の型 |
| --- | --- |
| `number` | `float` |
| `[number, number]` | `vec2` |
| `[number, number, number]` | `vec3` |
| `[number, number, number, number]` | `vec4` |

GLSL 配列には、明示的な `uniformArray.float/vec2/vec3/vec4` ヘルパーを使います。
配列長は GLSL の宣言と一致させる必要があり、シェーダーバインディングの構造として
扱われます。値の変更は native uniform fast path を通り、配列長の変更時だけ
バインディングが再構築されます。

```glsl
uniform float weights[3];
uniform vec2 control_points[2];
```

```ts
import {signal, uniformArray} from 'shoji_wm';

const phase = signal(0);

shaderStage(loadShader('./shaders/weighted.frag'), {
  uniforms: {
    weights: uniformArray.float([1, phase, 0.25]),
    control_points: uniformArray.vec2([
      [0, 0],
      [phase, 1],
    ]),
  },
});
```

配列全体、各スカラー要素、ベクトルの各成分のいずれにも Signal を指定できます。
空配列は使用できません。アニメーション中は配列長を固定し、異なる長さが必要な場合は
エフェクトまたは composition の構造を差し替えてください。

### シェーダーをアニメーションする

ユニフォームの値（各成分）は**シグナル**にできるので、変化する値を渡すことで
シェーダーをアニメーションできます――組み込みの `time` はありません。`phase` のような
ユニフォームを[アニメーション変数](./animations.md)や任意のシグナルで駆動し、エフェクトの
[`invalidate`](#無効化ポリシー) を`{kind: 'always'}`にして毎フレーム再描画させます。

ここでの `window` は合成関数の引数から渡されます（per-window エフェクトを返す
`COMPOSITOR.effect.window = (window) => {…}` でも同じように `window` を受け取れます）。

```tsx
import {animationVariable} from 'shoji_wm';

const pulse = animationVariable('pulse');

COMPOSITOR.window.composition = (window: WaylandWindow) => {
  // ループはどこかで開始します（例: ウィンドウを開いたとき）:
  // window.animation.start(pulse, {duration: 1000, repeat: 'ping-pong'});

  const glow = compileEffect({
    input: backdropSource(),
    invalidate: {kind: 'always'},
    pipeline: [
      shaderStage(loadShader('./shaders/glow.frag'), {
        uniforms: {
          phase: window.animation.variable(pulse), // シグナル → アニメーション
          intensity: 0.8,
        },
      }),
    ],
  });

  return (
    <ManagedWindow rect={window.position}>
      <ShaderEffect shader={glow}>
        <ClientWindow />
      </ShaderEffect>
    </ManagedWindow>
  );
};
```

```glsl
// shaders/glow.frag
uniform float phase;     // 0..1、TS からアニメーション
uniform float intensity;

vec4 shader_main(EffectContext effect) {
    vec4 source = texture2D(tex, effect.texture_uv);
    float k = 1.0 + intensity * phase;
    return vec4(source.rgb * k, source.a);
}
```

### 追加のテクスチャ

暗黙の `tex` 以外にも、テクスチャを追加でバインドできます。それぞれを
`uniform sampler2D` として宣言し、同じ名前で `textures` にソースハンドルを渡します。
レイヤー自身の内容をマスクとして使うのがこの方法です。

```glsl
// shaders/layer-mask.frag
uniform sampler2D layer_mask;
uniform float opacity_threshold;
uniform float mask_feather;

vec4 shader_main(EffectContext effect) {
    vec4 blurred = texture2D(tex, effect.texture_uv);      // 暗黙のソース
    float a = texture2D(layer_mask, effect.texture_uv).a;  // 追加のテクスチャ
    float mask = smoothstep(opacity_threshold - mask_feather,
                            opacity_threshold + mask_feather, a);
    return blurred * mask;
}
```

```ts
shaderStage(loadShader('./shaders/layer-mask.frag'), {
  textures: {layer_mask: layerSource()},
  uniforms: {opacity_threshold: 0.25, mask_feather: 0.04},
});
```

追加ソースも同じパディング付き作業テクスチャへ整列されます。追加ソースの可視内容の外側は
透明になるため、すべてのサンプラーで同じ `effect.texture_uv` 座標系を使用できます。

### GLSL ES 1.00 の注意点

この方言（デスクトップ GLSL より古い）で気をつけることをいくつか挙げます。

- テクスチャは **`texture2D(...)`** でサンプリングします（`texture(...)` ではない）。
- `precision highp float;` はすでに宣言済みです――再宣言しないでください。
- `in` / `out` / `layout` は使いません。`shader_main` を書いて `return` するだけです。
- `for` ループの回数は**定数**である必要があります（動的な長さは不可）。
- よく使う組み込み関数: `mix`・`clamp`・`smoothstep`・`step`・`length`・`dot`・
  `fract`・`floor`・`abs`・`min`・`max`・`sin`・`cos`・`pow`。

### 反復開発

シェーダーはパスでディスクから読み込まれるため、`.frag` を編集して設定をリロードすると
再コンパイルされます――フル再起動は不要です。まず `tex` をサンプリングしてそのまま返す
シェーダーから始め、1行ずつ変更していくのがおすすめです。
