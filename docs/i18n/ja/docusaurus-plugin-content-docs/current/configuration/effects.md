---
sidebar_position: 10
---

# エフェクト

ShojiWM は GPU シェーダーエフェクトを4箇所で実行でき、`COMPOSITOR.effect` で設定します。

| フィールド | 型 | 適用先 |
| --- | --- | --- |
| `background_effect` | `CompiledEffectHandle \| null` | クライアントが要求した領域の背後（`ext-background-effect-v1`） |
| `window` | `(window) => WindowEffectAssignment \| null` | トップレベルウィンドウごと |
| `layer` | `(layer) => LayerEffectAssignment \| null` | レイヤーシェルサーフェスごと（バー・ドック） |
| `popup` | `(popup) => PopupEffectAssignment \| null` | ポップアップごと（メニュー・ツールチップ） |

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

`windowSource`、`layerSource`、`popupSource`には
`{include: 'full' | 'root-surface'}`を指定できます。デフォルトは`'full'`です。

### ステージ

| ステージ | 目的 |
| --- | --- |
| `dualKawaseBlur({radius, passes})` | 高速で広いブラー |
| `shaderStage(shader, {uniforms, textures})` | カスタム GLSL フラグメントシェーダーを実行 |
| `noise({...})` | フィルムグレイン風のノイズを追加 |
| `save(name)` / `blend(input, {...})` | 中間結果の保存／合成 |
| `unit(effect)` | コンパイル済みの再利用可能なサブエフェクトを埋め込む |

`dualKawaseBlur`の`radius`はデフォルト`8`でサンプリング間隔を制御し、`passes`は
デフォルト`2`でダウン／アップサンプルの深さを制御します（`0`〜`8`に制限）。どちらを
変更しても`capturePadding`や`damagePadding`は自動変更されません。

`shaderStage` はシェーダー（パス、または `loadShader(path)` ハンドル）に加えて、
`uniforms`（シェーダーに渡す数値・色）と `textures`（名前で束縛する追加のソース
ハンドル）を取ります。

uniform値には数値または2／3／4成分の配列を指定でき、各成分をsignalにできます。
`tex`、`effect_texture_size_px`、`effect_content_rect_px`はコンポジターが使用する予約
bindingなので、独自のuniform名やtexture名には使えません。

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
    vec2 texture_uv;       // 作業テクスチャ全体の正規化座標
    vec2 texture_size_px;  // 作業テクスチャ全体の物理ピクセルサイズ
    vec4 content_rect_px;  // 可視内容の x, y, width, height
};
```

このおかげで、`shader_main` の中では次の組み込みが最初から使えます。

| 名前 | 型 | 意味 |
| --- | --- | --- |
| `effect.texture_uv` | `vec2` | キャプチャパディングを含むテクスチャ全体の正規化座標 |
| `effect.texture_size_px` | `vec2` | 作業テクスチャ全体の物理ピクセルサイズ |
| `effect.content_rect_px` | `vec4` | テクスチャ内の可視内容を `(x, y, width, height)` で表した矩形 |
| `tex` | `sampler2D` | このステージの入力。`texture2D(tex, effect.texture_uv)` でサンプリング |

すべての`*_px`値は物理ピクセルです。さらに次のヘルパーを利用できます。

| ヘルパー | 結果 |
| --- | --- |
| `effect_texture_px(effect)` | 作業テクスチャ全体における現在のフラグメント位置 |
| `effect_content_px(effect)` | 可視内容の左上を原点とした現在のフラグメント位置 |
| `effect_content_uv(effect)` | 可視内容上で`0.0`〜`1.0`、パディング部分では範囲外となる正規化座標 |
| `effect_texture_uv_from_content_px(effect, px)` | 可視内容基準の物理ピクセルをサンプリング用texture UVへ変換 |

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
    vec2 sample_px = effect_content_px(effect) + displacement_px;
    vec2 sample_uv = clamp(
        effect_texture_uv_from_content_px(effect, sample_px),
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
