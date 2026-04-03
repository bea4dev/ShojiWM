# Shared Edge Tree Plan

## Goal

SSD の layout 結果を `x/y/width/height` の集合として扱うのではなく、
**shared edge を正本にした resolved tree** として扱う。

これにより以下を安定させる。

- outer `Box` / `WindowBorder` / `<Window />` slot の physical size
- titlebar fill と client の接続
- `ShaderEffect` の clip / sample / visible rect
- border 太さ
- fractional scale 下の 1px breathing / gap / blur

## Current Problem

現在は layout 後に複数の経路で別々に量子化している。

- `PreciseLogicalRect`
- `SnappedLogicalRect`
- `ContentClip`
- physical render geometry

このため、論理 layout は固定でも render 時に

- `root:border`
- `WindowBorder`
- client

の physical edge が別々に `round` され、1px 単位で size が揺れる。

実ログでも layout size は固定なのに、render 側では

- root border: `2662x1708`, `2661x1708`, `2662x1709`
- window border: `2654x1700`, `2654x1701`
- client: `2646x1586`, `2646x1587`

が混在している。

## Target Architecture

### 1. Layout Tree

従来どおり、まずは論理 layout を計算する。

- `LogicalRect`
- `PreciseLogicalRect`
- padding / border / radius

ここは既存の `src/ssd/mod.rs` を大きく壊さない。

### 2. Shared Edge Tree

layout 完了後、render 前に **window 単位の shared edge tree** を構築する。

各 node は `rect` ではなく、以下を持つ。

- `left_edge`
- `top_edge`
- `right_edge`
- `bottom_edge`

各 edge は `EdgeId` で識別され、同じ端を共有する node 間で同一 `EdgeId` を参照する。

例:

- root content の `left`
- `WindowBorder` outer の `left`
- `WindowBorder` inner の `left`
- titlebar fill の `left`
- `<Window />` slot の `left`

は、共有されるべき関係に応じて同じ `EdgeId` か、親子差分 1 本分だけずれた `EdgeId` として表現する。

### 3. Edge Resolution

physical snap は rect 単位ではなく **edge 単位で一度だけ** 行う。

各 `EdgeId` に対して

- precise logical value
- snap policy
- resolved physical pixel

を求める。

その後、各 node の physical rect は

- `left_px = resolve(left_edge)`
- `right_px = resolve(right_edge)`
- `width_px = right_px - left_px`

で再構築する。

これで「同じ端なのに別々に量子化される」状態をなくす。

## Core Data Model

Rust 側で新設するもの:

```rust
struct EdgeId(u32);

enum EdgeAxis {
    Horizontal,
    Vertical,
}

enum EdgeSnapPolicy {
    Shared,
    Independent,
    PreserveThickness,
}

struct EdgeSpec {
    id: EdgeId,
    axis: EdgeAxis,
    logical: f32,
    policy: EdgeSnapPolicy,
}

struct EdgeRect {
    left: EdgeId,
    top: EdgeId,
    right: EdgeId,
    bottom: EdgeId,
}

struct ResolvedEdgeTreeNode {
    stable_key: String,
    rect: EdgeRect,
    content_rect: Option<EdgeRect>,
    clip_rect: Option<EdgeRect>,
}
```

`WindowDecorationState` には最終的に以下を持たせる。

- layout tree
- shared edge tree
- resolved physical edge map

## Snap Policy

すべての edge を単純に同じ規則で丸めるのではなく、役割ごとに policy を分ける。

### `Shared`

shared edge として完全一致させる。

対象:

- root content と `WindowBorder` outer/inner の接続部
- titlebar fill と `<Window />` slot の left/right
- `ShaderEffect` の clip rect
- client slot の visible rect

### `PreserveThickness`

border 幅や radius の見た目安定性を優先し、左右または上下の edge を paired に解決する。

対象:

- `WindowBorder`
- outer `Box` border
- button border

これは「片側だけ shared、もう片側だけ independent」にせず、
**厚みを持つ装飾の内外 edge を一組で解く**ために使う。

### `Independent`

共有不要な自由要素向け。

対象:

- 通常の `Label`
- `AppIcon`
- shared edge を要求しない装飾要素

## Construction Rules

### Root

- root outer rect の 4 辺を base edge として生成
- root content rect は border/padding を経由した別 edge を生成

### `WindowBorder`

- outer edge は親 content edge を参照
- inner edge は border width を使って派生
- outer/inner の paired relation を記録し `PreserveThickness` で解決

### `Box`

- 通常の box は親から受けた edge をそのまま使うか、padding 分だけ派生 edge を作る
- split / flex layout は child ごとに shared edge を再配線する

例:

- row layout の sibling は top/bottom を共有
- left child の right と right child の left は gap を挟んだ別 edge にする

### `<Window />`

- rect ではなく **slot edge ref** を持つ
- client の `ContentClip`
- hit-test 用 rect
- shader / fill との接続

すべて同じ slot edge から導出する

### `ShaderEffect`

- visible rect
- sample rect
- clip rect

を別々の生値で持たず、**どの edge set を参照するか** を持つ。

## User-Facing API

内部だけ shared edge 化しても、`ShaderEffect` などが任意の geometry を選べないと活かしきれない。
そのため TS 側にも geometry 参照 API を用意する。

### Design Principles

- 何も指定しなければ既存挙動と近い default
- 上級者は geometry source を明示できる
- rect 値を直接書くのではなく、**node の geometry を参照**する
- **丸め方そのものは compositor が自動で決める**
- `ShaderEffect` からは、**解決済み geometry を参照できる**ようにする

### Proposed Geometry Handle API

各 node の `id` を geometry 参照に使う。

```tsx
<Box id="titlebar">
  ...
</Box>
```

```tsx
<ShaderEffect
  shader={...}
  geometry={{ from: "titlebar", box: "border-box" }}
  clip={{ from: "window-frame", box: "content-box" }}
>
  ...
</ShaderEffect>
```

提案 props:

```ts
type GeometryReference =
  | { from: "self"; box?: "border-box" | "content-box" | "clip-box" | "window-slot" }
  | { from: string; box?: "border-box" | "content-box" | "clip-box" | "window-slot" };

interface ShaderEffectProps {
  shader: CompiledEffectHandle;
  geometry?: GeometryReference;
  clip?: GeometryReference;
  sample?: GeometryReference;
}
```

ここで指定するのは **snap policy ではなく geometry の参照先** だけにする。

- どの node の box を使うか
- どの clip を使うか
- どの sample rect を使うか

は指定できるが、

- `SharedEdges`
- `OriginAndSize`
- floor / ceil / round の違い

のような内部の丸め規則は API として露出しない。

### Why String `id`

既に `BoxProps` / `ShaderEffectProps` / `WindowBorderProps` は `id?: string` を持つ。
そのため新しい handle object を導入するより、まずは `id` ベース参照が低コスト。

### Reserved References

ユーザーがよく使うものは予約名または sugar を用意する。

- `"self"`
- `"window-slot"`
- `"window-border"`
- `"root"`

例:

```tsx
<ShaderEffect
  shader={glass}
  geometry={{ from: "window-slot" }}
  clip={{ from: "window-border", box: "content-box" }}
/>
```

### Default Behavior

`ShaderEffect` の default は次に寄せる。

- `geometry`: self border-box
- `clip`: nearest shared ancestor content-box
- `sample`: `clip` と同じ

これで既存 tree を大きく壊さず、必要時だけ明示指定できる。

### Shader Runtime View

`ShaderEffect` の利用者に必要なのは「どう丸めるか」ではなく、
**最終的にどう丸められたか** である。

そのため shader には、geometry reference を解決した結果を uniform として渡す。

候補:

- `u_resolved_rect_px`
- `u_resolved_clip_rect_px`
- `u_resolved_sample_rect_px`
- `u_resolved_rect_logical`
- `u_output_scale`
- `u_device_pixel_ratio`

必要なら将来的に、

- `u_window_slot_rect_px`
- `u_window_border_rect_px`
- `u_root_rect_px`

のような予約 uniform も追加できる。

重要なのは、shader 作者が

- 「この effect は window-slot に揃えたい」
- 「clip は window-border content-box を使いたい」

を指定できれば十分であり、
**丸めアルゴリズム自体を意識しなくてよい**こと。

### Browser Analogy

ブラウザも基本的には同じ考え方で、
開発者に pixel snapping policy を指定させるのではなく、
解決済みの box を参照させる。

ShojiWM でも同様に、

- 丸めは compositor の責務
- `ShaderEffect` は解決済み geometry を参照して使う

という責務分離を採用する。

## Backend Changes

### `src/ssd/mod.rs`

- layout output を `ResolvedLayoutRect` 中心から `EdgeRect` 中心へ拡張
- split / flex / padding / border がどの edge を共有するかを記録

### `src/ssd/integration.rs`

- `WindowDecorationState` に shared edge tree を保持
- `ContentClip` を raw rect ではなく edge reference ベースに再設計
- node `id` と stable key から geometry reference を解決する index を追加

### `src/backend/visual.rs`

- `SnappedLogicalRect` helper 群を edge map ベースに再整理
- `RectSnapMode` は node 単位ではなく edge policy へ寄せる

### `src/backend/decoration.rs`

- border / fill / rounded clip の geometry を shared edge から再構築
- `WindowBorder` inner hole は edge tree の結果を使い、個別補正しない

### `src/backend/clipped_surface.rs`

- client の visible rect / clip rect / projected size を slot edge から決定
- 現在の Hyprland 型 UV 補正は残しつつ、shared edge 化後は fallback 扱いにする

### `src/backend/shader_effect.rs`

- effect spec に `geometry_ref` / `clip_ref` / `sample_ref` 由来の rect を渡せるようにする
- shader uniform として resolved geometry 群を渡せるようにする
- effect cache key に geometry reference 解決結果を含める

### `packages/shoji_wm/src/types.ts`

- `GeometryReference`
- `ShaderEffectProps.geometry`
- `ShaderEffectProps.clip`
- `ShaderEffectProps.sample`

を追加

### `src/ssd/bridge.rs`

- 上記 props の serialize / deserialize 対応

## Migration Plan

### Phase 1: Internal Edge Graph

目的:

- render に使わず、まずは edge graph を構築して debug 出力する

完了条件:

- root / `WindowBorder` / `Window` slot の edge id をログで確認できる
- 「共有されるべき edge」が意図通り同じ `EdgeId` になっている

### Phase 2: Border And Slot

目的:

- root border
- `WindowBorder`
- `<Window />` slot

だけ shared edge から physical rect を作る

完了条件:

- outer box / `WindowBorder` / client の physical breathing が消える
- border thickness regression が起きない

### Phase 3: Fill And ShaderEffect

目的:

- titlebar fill
- backdrop shader
- `ShaderEffect`

も shared edge へ移行

完了条件:

- `fill_client_edge_delta` が恒常的に 0
- `ShaderEffect` と client / border の接続が安定

### Phase 4: User Geometry References

目的:

- `id` ベース geometry reference を公開

完了条件:

- `<ShaderEffect geometry=... clip=... sample=... />` が TS から使える
- `window-slot` / `self` / node `id` 参照が通る
- shader 内で resolved geometry uniform を参照できる

### Phase 5: Cleanup

目的:

- 個別補正コードの削除

対象:

- ad-hoc な `clip_size_delta_px` 分岐
- render 後段の edge repair
- component ごとの独自 snap 分岐の一部

## Debugging And Validation

追加すべきログ:

- node -> `EdgeRect`
- `EdgeId` -> precise logical value
- `EdgeId` -> physical px
- geometry reference 解決結果

追加すべき検証:

- 同じ tree を 1px ずつ移動しても size が不変
- border thickness が不変
- `ShaderEffect` / fill / client の edge delta が 0
- fractional scale `1.25`, `1.5`, `1.75`, `2.0` で再現テスト

追加すべきテスト:

- edge sharing unit test
- split layout edge assignment test
- `WindowBorder` thickness stability test
- translation stability golden test

## Non-Goals

今回の計画では以下は目的にしない。

- CSS 的な constraint system の全面導入
- arbitrary anchor layout の追加
- Wayland client buffer 側の仕様変更
- Hyprland 型 UV 補正の即時削除

## Practical Decision

最初の実装は **layout system の全面書き換えではなく、layout 後に shared edge tree を作る方式** で進める。

理由:

- 既存 TS DSL を壊しにくい
- 既存の flex / box layout を流用できる
- 問題の本丸が render 前の edge quantization 不一致だから

この方針で、まずは root / `WindowBorder` / `<Window />` slot を shared edge tree に載せる。
