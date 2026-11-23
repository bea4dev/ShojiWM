---
sidebar_position: 10.61
---

# 出力の合成リファレンス

[出力の合成](./output-composition.md)のノードとヘルパーの一覧です。すべて `shoji_wm`
から import できます。*signal* と書いた props は値でも signal でも渡せ、signal を
渡すとその変化で合成が再評価されます。

## `COMPOSITOR.rendering.composition`

```ts
COMPOSITOR.rendering.composition = (output: OutputInfo) => CompositionRenderable;
```

出力ごとに呼ばれます。以下のノードからなる JSX ツリーを返してください（フラグメントと
配列は自由に入れ子にでき、`null`・`false`・`undefined` は何も描きません）。既定の
重なりにするには未設定のままにするか、`<DefaultComposition />` を返します。ノードが
1 つも無い合成は何も描かないので、`null` ではなく既定を返してください。

## ノード

子要素は奥から手前の順で、後の兄弟ほど上に描かれます。

### `<Layers>`

出力のレイヤーシェルサーフェス。

| Prop | 型 | |
| --- | --- | --- |
| `layers` | *signal* `LayerName \| LayerName[]` | `"background"`・`"bottom"`・`"top"`・`"overlay"` を奥から手前の順で。 |

各レイヤー内のサーフェスの順（新しいものが手前）は保たれます。レイヤーは複数のノードに
分けても、どんな順に並べても構いません（Top をウィンドウの下に置くなど）。

### `<Windows>`

| Prop | 型 | |
| --- | --- | --- |
| `windows` | *signal* `(WaylandWindow \| string)[]` | 指定したウィンドウ（オブジェクトか ID）だけを重なり順で描きます。**非表示でも描きます**（表示時と同じ姿で）。省略すると出力自身の重なり。 |
| `offsetX`, `offsetY` | *signal* `number` | ウィンドウをずらす量（論理ピクセル）。 |

`windows` を省略したノードは出力自身の重なりで、出力が表示しているものを閉じかけの
ウィンドウと装飾ポップアップ込みで描きます。フルスクリーンのファストパスとダイレクト
スキャンアウトはこの重なりにだけ、合成のルートにあるときだけ働きます。`windows` を
指定した場合、合成の範囲外にあるウィンドウは描かれません。

この重なりの一番上のウィンドウがフルスクリーンのとき、このノードより後に並べた
`"top"` レイヤーは描かれません。ポインターと同じく、バーはウィンドウの後ろに回ります。
手前に残るのは `"overlay"` だけです。出力全体を覆う X11 ウィンドウ（xwayland-satellite
経由）も、KWin や Mutter と同じくフルスクリーンとして扱います。多くのゲームの
「ボーダーレスフルスクリーン」はこの形です。

### `<LayerPopups>`

全レイヤーのレイヤーシェルのポップアップ（バーのメニューやツールチップ）。props は
ありません。

### `<TextureView>`

レンダーテクスチャを平面で描きます。

| Prop | 型 | |
| --- | --- | --- |
| `texture` | `RenderTexture` | `renderTexture` の戻り値。 |
| `x`, `y`, `width`, `height` | *signal* `number` | 合成内の位置（論理ピクセル）。省略すると全体。テクスチャは引き伸ばされます。 |
| `opacity` | *signal* `number` | 0〜1（既定 1）。 |

### `<Solid>`

単色の塗り。

| Prop | 型 | |
| --- | --- | --- |
| `color` | *signal* `CompositionColor` | `"#rgb"`・`"#rgba"`・`"#rrggbb"`・`"#rrggbbaa"`、または 0〜1 の `[r, g, b, a]`。 |
| `x`, `y`, `width`, `height` | *signal* `number` | `<TextureView>` と同じ。 |

### `<Scene3D>`

`<Plane>` からなる 3D シーン。深度バッファ付きでオフスクリーンに描かれ、合成の 1 層として
描かれます。

| Prop | 型 | |
| --- | --- | --- |
| `camera` | *signal* `Camera` | `{ projection, view }` の行列。[カメラ](#カメラ)を参照。 |
| `x`, `y`, `width`, `height` | *signal* `number` | シーンのビューポート。省略すると全体。 |
| `clearColor` | *signal* `CompositionColor` | 平面の背後（既定は透明）。 |
| `antialias` | *signal* `boolean` | 4x マルチサンプルのエッジ（既定 true）。 |

平面は奥から手前の順に描かれ、半透明の平面は奥のものの上にブレンドされます。
テクスチャの完全に透明な部分（ウィンドウのテクスチャの影の余白など）は奥のものを
隠しません。同じ深さの平面はちらつくので、重なる平面は z をずらしてください。

### `<Plane>`

テクスチャ付きの矩形。`<Scene3D>` の中でだけ使えます。

| Prop | 型 | |
| --- | --- | --- |
| `texture` | `RenderTexture` | 平面に映すもの。 |
| `width`, `height` | *signal* `number` | ワールド単位のサイズ（平面の原点が中心）。 |
| `transform` | *signal* `Transform3D \| Mat4` | ワールドでの配置（既定は単位行列: 原点でカメラの方を向く）。 |
| `opacity` | *signal* `number` | 0〜1（既定 1）。 |
| `doubleSided` | *signal* `boolean` | 裏面も描く（既定 true）。 |

### `<DefaultComposition />`

既定の重なり: `Layers[background, bottom]`、`Windows`、`Layers[top, overlay]`、
`LayerPopups`。

## `renderTexture(options)`

```ts
renderTexture({
  content: CompositionRenderable,   // 描くノード
  key?: string,                     // 再評価をまたいで GPU テクスチャを保つ
  width?: number, height?: number,  // 論理サイズ（既定は出力のもの）
  scale?: number,                   // 画素密度（既定は出力の scale）
  clearColor?: CompositionColor,    // 既定は透明
}): RenderTexture
```

中身は出力と同じ配置で描かれます。レイヤーとウィンドウは出力の左上を基準にいつもの
位置に現れ、テクスチャのサイズで切り取られます。何度使っても 1 フレームに 1 回だけ、
変化した部分だけ描かれます。`key` が無いと、テクスチャは呼び出し順で区別されます。

## 3D ヘルパー

### `transform3d(matrix?)`

`Transform3D` を始めます。CSS の `transform` と同じ要領で、各呼び出しはそれまでの
変換のローカル空間で適用されます。角度は度です。

| メソッド | |
| --- | --- |
| `translate(x, y = 0, z = 0)` | 移動。 |
| `rotateX(deg)`, `rotateY(deg)`, `rotateZ(deg)` | 軸まわりの回転。 |
| `scale(x, y = x, z = 1)` | 拡大縮小。 |
| `multiply(other)` | 別の `Transform3D` か `Mat4` を適用。 |
| `.matrix` | 列優先の `Mat4`。 |

変換は不変なので、共通の土台から枝分かれできます:

```ts
const cube = transform3d().translate(0, 0, -half).rotateY(angle);
const front = cube.translate(0, 0, half);
const right = cube.rotateY(90).translate(0, 0, half);
```

### カメラ

| ヘルパー | |
| --- | --- |
| `screenCamera(output \| { width, height }, { fov = 45, distance = 0 })` | z = 0 を出力に 1:1 で写します。`distance` でカメラを引きます（ズームアウト）。 |
| `perspective(fovY, aspect, near, far)` | OpenGL 式の透視投影行列。`fovY` は度。 |
| `lookAt(eye, target, up = [0, 1, 0])` | `eye` から `target` を見るカメラのビュー行列。 |
| `multiplyMat4(a, b)` | 列優先の `a × b`。 |
| `outputLogicalSize(output)` | 出力の論理サイズ `{ width, height }`。 |

カメラを自前で組む例:

```ts
const { width, height } = outputLogicalSize(output);
const camera = {
  projection: perspective(50, width / height, 1, 10_000),
  view: lookAt([0, 300, 1600], [0, 0, 0]),
};
```

### 当たり判定

| ヘルパー | |
| --- | --- |
| `projectPoint(camera, viewport, [x, y, z])` | ワールドの点が写る位置 `{ x, y, depth }`（ビューポート左上からの論理ピクセル。`depth` は小さいほど手前）。カメラの後ろなら `null`。 |
| `pickPlane(camera, viewport, planes, x, y)` | `(x, y)` の下で一番手前の平面の添字、無ければ `null`。`planes` には `<Plane>` と同じ `width`・`height`・`transform` を渡します。 |

`viewport` は `<Scene3D>` の `{ width, height }` です。
[入力グラブ](./keybindings-and-pointer.md#入力グラブ)と組み合わせると 3D の配置を
クリックできるようになります:

```ts
const { width, height } = outputLogicalSize(output);
const hit = pickPlane(screenCamera(output), { width, height }, planes,
  event.position.x - output.position.x, event.position.y - output.position.y);
```

## 型

| 型 | |
| --- | --- |
| `CompositionRenderable` | 合成関数の戻り値。 |
| `RenderTexture` | `renderTexture` の戻り値。 |
| `Camera` | `{ projection: Mat4; view: Mat4 }`。 |
| `Mat4` | 列優先の 16 個の数（WebGL/glMatrix と同じ並び）。 |
| `LayerName` | `"background" \| "bottom" \| "top" \| "overlay"`。 |
| `CompositionColor` | 16 進文字列（`"#rgb"`〜`"#rrggbbaa"`）か 0〜1 の `[r, g, b, a]`（ストレートアルファ）。 |
