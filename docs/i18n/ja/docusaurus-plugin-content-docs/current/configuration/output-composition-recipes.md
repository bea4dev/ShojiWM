---
sidebar_position: 10.62
---

# 出力の合成レシピ集

[出力の合成](./output-composition.md)で作る演出の完成形です。どれも演出していない間は
`null` を返す関数になっているので、そのときは既定の重なりに戻します。こうすると
フルスクリーンのファストパスも保たれます:

```tsx
COMPOSITOR.rendering.composition = (output) =>
  crossfade(output) ?? <DefaultComposition />;
```

合成関数そのものからは `null` ではなく既定を返してください。ノードが 1 つも無い合成は
何も描きません。

以下の名前はすべて `shoji_wm` から import できます。

## 任意の 2 つのビュー間のキューブ

`examples/output-composition/cube-transition.tsx` は、任意の 2 つの合成（2 つの
ワークスペースなど）の間でキューブを回します:

```tsx
import { COMPOSITOR, DefaultComposition, Layers, Windows } from "shoji_wm";
import { createCubeTransition } from "./cube-transition";

const cube = createCubeTransition({ duration: 700 });
COMPOSITOR.rendering.composition = (output) =>
  cube.compose(output) ?? <DefaultComposition />;

// 切り替えるとき（ウィンドウ自体はアニメーションなしで切り替える）:
cube.start(outputName, {
  from: <><Layers layers={["background", "bottom"]} /><Windows windows={fromIds} /></>,
  to: <><Layers layers={["background", "bottom"]} /><Windows windows={toIds} /></>,
  direction: 1,
});
```

## バー以外を暗くする

テクスチャは要りません。ウィンドウとバーの間に半透明の塗りを挟みます。

```tsx
const [dimmed, setDimmed] = signal(false);

COMPOSITOR.rendering.composition = () => (
  <>
    <Layers layers={["background", "bottom"]} />
    <Windows />
    {dimmed() && <Solid color="#00000099" />}
    <Layers layers={["top", "overlay"]} />
    <LayerPopups />
  </>
);
```

ルートに `<Windows />` が残っているので、塗りが無い間はフルスクリーンのファストパスも
働きます。

## クロスフェード

2 つのテクスチャビューで、あるビューのスナップショットから別のビューへフェードします:

```tsx
const [fade, setFade] = signal<number | null>(null); // 0 → 1

function crossfade(output: OutputInfo) {
  const t = fade();
  if (t === null) return null;
  const from = renderTexture({ key: "fade-from", content: <Windows windows={fromIds} /> });
  const to = renderTexture({ key: "fade-to", content: <Windows windows={toIds} /> });
  return (
    <>
      <Layers layers={["background", "bottom"]} />
      <TextureView texture={from} opacity={1 - t} />
      <TextureView texture={to} opacity={t} />
      <Layers layers={["top", "overlay"]} />
      <LayerPopups />
    </>
  );
}
```

`fade` は出力の `createPoll` で動かし、終わったら `null` に戻します。

## ズーム

デスクトップのテクスチャを画面上の 1 点を中心に拡大します。3D シーンは平面を出力の
範囲に切り取るので、倍率はいくらでも構いません:

```tsx
function zoomed(output: OutputInfo) {
  const k = zoom(); // 1 = 等倍
  if (k <= 1) return null;
  const { width, height } = outputLogicalSize(output);
  const desktop = renderTexture({ key: "zoom", content: <DefaultComposition />, scale: 2 });
  // 注目点のワールド座標（原点は中央、+Y が上）。
  const cx = focusX() - width / 2;
  const cy = height / 2 - focusY();
  return (
    <Scene3D camera={screenCamera(output)}>
      <Plane texture={desktop} width={width} height={height}
        transform={transform3d().translate(cx * (1 - k), cy * (1 - k)).scale(k)} />
    </Scene3D>
  );
}
```

`scale: 2` でテクスチャを 2 倍の画素密度で描くので、2 倍までは文字も鮮明です。入力は
拡大されず、ウィンドウの実際の位置に届きます。

## ウィンドウごとのテクスチャ

テクスチャには 1 つのウィンドウだけを収めることもできます。テクスチャをウィンドウの
大きさにし、`offsetX`/`offsetY` でウィンドウをテクスチャの角に合わせます。大きさには
装飾込みのウィンドウ全体である `window.rect` を使い（`window.position` はタイトルバーを
含まないクライアント領域です）、影のぶんの余白を取っておきます。

```tsx
const MARGIN = 48;

function windowTexture(output: OutputInfo, window: WaylandWindow) {
  const { x, y, width, height } = window.rect;
  return renderTexture({
    key: `window-${window.id}`,
    width: width + MARGIN * 2,
    height: height + MARGIN * 2,
    content: (
      <Windows
        windows={[window]}
        offsetX={output.position.x - x + MARGIN}
        offsetY={output.position.y - y + MARGIN}
      />
    ),
  });
}
```

こうしたテクスチャはそのウィンドウが変わったときだけ描き直されるので、十数枚あっても
止まっている間はほとんどコストがかかりません。ID 指定なので、最小化中や別ワーク
スペースのウィンドウも映ります。

### 3D ウィンドウスイッチャー

ウィンドウごとのテクスチャがあれば、Windows Vista の「フリップ 3D」風のスイッチャーは
平面の列として作れます。デフォルト設定に同梱されています:
`packages/config/src/flip-3d.tsx`（`Super` + `Tab`、[Flip 3D](./default-config.md#flip-3d) 参照）。
構成要素:

- **配置。** 各ウィンドウはそのテクスチャの `<Plane>` で、`screenCamera(output)` の
  `<Scene3D>` 1 つに並べます。ウィンドウの*実位置の姿勢*（中心への平行移動、回転なし、
  倍率 1）では平面がちょうど画面上のウィンドウに重なり、*スロットの姿勢*では列の中に
  入ります。開くときは実位置→スロット、閉じるときはスロット→実位置へ補間するので、
  デスクトップがそのまま列に変形して戻ります。補間するのは行列ではなく数値（位置・角度・
  倍率・不透明度）です。
- **入力。** 開いている間は[入力グラブ](./keybindings-and-pointer.md#入力グラブ)で
  キーボード・クリック・ホイール・スワイプを受け取り、
  [`pickPlane`](./output-composition-reference.md#当たり判定) でクリックをウィンドウに
  変換します。`Super` + `Tab` から押し続けている `Super` を離すと、キーの離しとして届きます。
- **切り替え。** 選んだウィンドウのアクティブ化は、ウィンドウマネージャー自身の
  アニメーション（別ワークスペースへの切り替え、最小化からの復帰）*なしで*行い、
  平面をウィンドウの新しい位置へ飛ばします。
- **引き継ぎ。** すべての平面がウィンドウの上に重なったら `<DefaultComposition />` に
  戻します。ease-out の曲線なら終わりより少し前に重なります。

どれも列の形には依存しません。デフォルト設定ではこれらを `window-switcher.tsx` にまとめ、
配置とその入力だけを別ファイルにしています。列は `flip-3d.tsx`、平面のオーバービュー
グリッドは `window-grid.tsx` です（[ウィンドウグリッド](./default-config.md#ウィンドウグリッド)）。

**ウィンドウごとのテクスチャの中のブラー。** テクスチャに単独で描いたウィンドウの下には
何も無いので、そのウィンドウの backdrop ブラー（ガラスのタイトルバーなど）は空を
ぼかしつつ、ウィンドウ 1 枚ごとにブラーのコストがかかります。スイッチャーを開いている間は、
装飾がエフェクトを選ぶ箇所で signal を読んでエフェクトを外し、エフェクトの最後に強さを
signal で受け取る段を付けて、引き継ぎの後にじわっと戻します:

```glsl
// backdrop-fade.frag: 0 = エフェクト無し（透明）、1 = そのままのエフェクト
uniform float strength;
vec4 shader_main(EffectContext effect) {
    vec4 color = texture2D(tex, effect.texture_uv);
    color.a = 1.0;
    return color * strength;
}
```

```tsx
const glass = compileEffect({
  input: backdropSource(),
  alpha: "preserve",
  pipeline: [
    dualKawaseBlur({ radius: 4, passes: 2 }),
    shaderStage(loadShader("./src/effect/backdrop-fade.frag"), {
      uniforms: { strength: backdropStrength },
    }),
  ],
});
// 装飾の中で:
blurSuspended() ? <Box style={titlebar}>…</Box> : <ShaderEffect shader={glass} style={titlebar}>…</ShaderEffect>
```
