---
sidebar_position: 8
---

# SSD コンポーネント

これらは [`COMPOSITOR.window.composition`](./window-composition.md) の内側で組み立てて
ウィンドウ装飾を描くための部品です。`shoji_wm` から import します。

```tsx
import {Box, Label, Button, AppIcon, Image, Popup, ShaderEffect, WindowBorder} from 'shoji_wm';
```

`<ManagedWindow/>` と `<ClientWindow/>` は
[ウィンドウの合成](./window-composition.md) ページで解説しています。

## 共通の prop

すべてのコンポーネントが受け付けます（`ComponentProps` 由来）。

| Prop | 型 | 意味 |
| --- | --- | --- |
| `children` | ノード | 子コンポーネント |
| `style` | `SSDStyle` | 視覚スタイル。[スタイルリファレンス](#スタイルリファレンス) を参照 |
| `id` | `string` | ターゲット無効化のための安定したノード id |
| `onHoverChange` | `(hovered: boolean) => void` | ポインターの出入り |
| `onActiveChange` | `(active: boolean) => void` | 押下／解放 |
| `paint` | `PaintShaderHandle` | 組み込みの背景・枠を置き換え。[ペイントシェーダー](./paint.md)を参照 |
| `overlay` | `PaintShaderHandle` | 子要素より上に描画。[ペイントシェーダー](./paint.md)を参照 |

すべての `style` 値（および多くの prop）は、素の値かシグナルのどちらも受け付けるので、
リアクティブに更新されます。

---

## `<Box/>`

子要素を水平または垂直に整列するフレックスボックス風コンテナです。

| Prop | 型 | 意味 |
| --- | --- | --- |
| `direction` | `"row" \| "column" \| "horizontal" \| "vertical"` | レイアウト軸（デフォルト `"row"`） |
| `split` | `Direction` | 2 パネルレイアウトの分割方向 |
| `style` | `SSDStyle` | スタイル |

```tsx
<Box direction="row" style={{gap: 8, padding: 4, alignItems: 'center'}}>
  <AppIcon icon={window.icon} style={{width: 16, height: 16}} />
  <Label text={window.title} style={{flexGrow: 1}} />
</Box>
```

## `<Label/>`

テキスト文字列を描画します。

| Prop | 型 | 意味 |
| --- | --- | --- |
| `text` | `string`（またはシグナル） | 表示するテキスト |
| `style` | `SSDStyle` | `fontSize`・`fontWeight`・`fontFamily`・`color`・`textAlign`・`lineHeight` でフォントと色を指定 |

```tsx
<Label
  text={window.title}
  style={{color: '#f5f7fa', fontSize: 13, fontWeight: 600, fontFamily: ['Noto Sans CJK JP', 'Noto Color Emoji']}}
/>
```

## `<Button/>`

クリックでアクションをトリガーするプレス可能な領域です。

| Prop | 型 | 意味 |
| --- | --- | --- |
| `onClick` | `() => void` または `WindowActionDescriptor` | クリック時のアクション |
| `onHoverChange` | `(hovered: boolean) => void` | 視覚フィードバック用のホバー追跡 |
| `style` | `SSDStyle` | スタイル |

`onClick` はコールバック、または組み込みウィンドウ操作用に `windowAction(...)` が返す
ディスクリプタを受け付けます。操作は `"close"`・`"maximize"`・`"unmaximize"`・
`"minimize"`・`"fullscreen"`・`"unfullscreen"` です。

```tsx
import {Button, windowAction} from 'shoji_wm';

// 組み込みアクション
<Button onClick={windowAction('close')} style={{width: 12, height: 12}} />

// カスタムハンドラ＋ホバーフィードバック
const [hover, setHover] = useState(false);
<Button
  onHoverChange={setHover}
  onClick={() => window.minimize()}
  style={{width: 16, height: 16, borderRadius: 8, background: hover((h) => h ? '#FFFFFF40' : '#FFFFFF20')}}
/>
```

## `<AppIcon/>`

ウィンドウのアプリケーションアイコンを描画します。

| Prop | 型 | 意味 |
| --- | --- | --- |
| `icon` | `WindowIcon \| undefined`（またはシグナル） | `window.icon` を渡すとリアクティブに更新 |
| `fallback` | `string` | アイコンが見つからないときに描く画像ファイル（`<Image src>` と同じく解決） |
| `style` | `SSDStyle` | サイズ指定 |

```tsx
<AppIcon icon={window.icon} style={{width: 16, height: 16}} />
```

## `<Image/>`

ファイルパス（設定パッケージルートからの相対）またはリアクティブなソースから画像を
表示します。

| Prop | 型 | 意味 |
| --- | --- | --- |
| `src` | `string`（またはシグナル） | 画像パス |
| `fit` | `"contain" \| "cover" \| "fill"` | 画像がボックスを満たす方法 |
| `style` | `SSDStyle` | サイズ／配置 |

```tsx
<Image src="./assets/x.svg" style={{width: 16, height: 16, pointerEvents: 'none'}} />
```

## ShaderEffect

`<ShaderEffect/>` は子要素が占める領域にコンパイル済み GPU エフェクトを適用する
コンテナです。`shader` の作り方は [エフェクト](./effects.md) を参照してください。

| Prop | 型 | 意味 |
| --- | --- | --- |
| `shader` | `CompiledEffectHandle` | 描画するコンパイル済みエフェクト |
| `direction` | `Direction` | 子要素のレイアウト軸（`<Box/>` と同様） |
| `style` | `SSDStyle` | スタイル |

```tsx
<ShaderEffect shader={frostedGlass} direction="row" style={{height: 28, paddingX: 8, alignItems: 'center'}}>
  <Label text={window.title} />
</ShaderEffect>
```

## WindowBorder

`<WindowBorder/>` は `<ClientWindow/>` の周囲に置き、ボーダーを描画してインタラクティブな
リサイズの当たり判定を提供するクロムコンテナです。

| Prop | 型 | 意味 |
| --- | --- | --- |
| `style` | `SSDStyle` | `border`・`borderRadius`・`background` などでボーダーを指定 |
| `interaction` | `WindowBorderInteraction` | リサイズの当たり判定 |

`interaction.resizeHitArea` は単一の数値か、`{edgePx?, cornerPx?, insetPx?}` です。
リサイズのつかみ領域は、見えているボーダーにフレームの外側 `edgePx` を加えた範囲です。
そのため、縁の近くのクライアント内容には被りません。`cornerPx` は斜めリサイズになる
コーナー部分が各辺に沿ってどこまで伸びるかで、つかみ領域の外側の角から測ります。
`insetPx` はつかみ領域をボーダーの内側へその分だけ広げます。ボーダーの子要素で描いた
枠（Windows のような太いタイトルバーや左右の枠）を外縁からリサイズしたいときに使い、
クライアントがポインターを受け取れるよう枠の幅より狭くしてください。
`resizeHitArea` を指定しない場合は、ボーダー自体だけがリサイズ領域になります。指定した
場合は、ボーダーのない `<WindowBorder>` もリサイズできます。

```tsx
<WindowBorder
  style={{border: {px: 2, color: borderColor}, borderRadius: 10}}
  interaction={{resizeHitArea: {edgePx: 8, cornerPx: 14}}}
>
  <ClientWindow />
</WindowBorder>
```

## `<Popup/>`

親の隣に表示し、**ウィンドウの外**に描くコンテンツです。タイトルバーのボタンの
ツールチップや、ボタン付きのメニューに使います。属するコンポーネントの中に書き
（親が *基準（アンカー）* になります）、そのコンポーネントのシグナルをそのまま
使えます。仕組みはブラウザの
[Popover API](https://developer.mozilla.org/ja/docs/Web/API/Popover_API) に倣っています。

### ツールチップ

```tsx
import {Box, Button, Label, Popup, windowAction} from 'shoji_wm';

<Button onClick={windowAction('maximize')} style={{width: 16, height: 16}}>
  <Popup trigger="hover" openDelay={500} placement="bottom" offset={6}>
    <Box style={{background: '#1e1e2ef0', borderRadius: 6, paddingX: 10, paddingY: 4}}>
      <Label text="最大化" style={{fontSize: 12, color: '#f5f7fa'}} />
    </Box>
  </Popup>
</Button>
```

`trigger="hover"` は、ボタンにポインターを `openDelay` ms 置くと開き、離れて
`closeDelay` ms 経つと閉じます。

### ボタン付きのメニュー

```tsx
<Button onClick={toggleMaximize}>
  <Popup trigger="hover" mode="auto" openDelay={500} closeDelay={200}>
    <Box direction="row" style={{gap: 6, padding: 6}}>
      <Button onClick={() => snap(window, 'left')}>…</Button>
      <Button onClick={() => snap(window, 'right')}>…</Button>
    </Box>
  </Popup>
</Button>
```

`mode="auto"` ではポップアップが入力を受けるので、中のボタンが押せます。基準から
ポップアップへポインターを移しても、ポップアップ内では基準もホバー中とみなされ、
間の隙間は `closeDelay` が吸収します。外側を押す・Esc・別の `"auto"` ポップアップが
開く、のいずれかで閉じます。

### 自分で状態を持つ、クリックで開くメニュー

```tsx
const [open, setOpen] = useState(false);

<Button onClick={() => setOpen(!open())}>
  <AppIcon icon={window.icon} />
  <Popup mode="auto" open={open} onOpenChange={(next, reason) => setOpen(next)}>
    <Button onClick={() => { window.close(); setOpen(false); }}>…</Button>
  </Popup>
</Button>
```

短く書くなら `<Popup trigger="click" mode="auto">` です。

### Props

| Prop | 型 | 意味 |
| --- | --- | --- |
| `open` | `boolean`（またはシグナル） | 表示するか（既定 `true`）。`trigger` 使用時は使いません |
| `trigger` | `"hover" \| "click"` | ポップアップが自分で開閉する（後述） |
| `openDelay` | `number` | `trigger="hover"`: 基準に置いてから開くまでの ms（既定 `500`） |
| `closeDelay` | `number` | `trigger="hover"`: 離れてから閉じるまでの ms（既定 `200`） |
| `mode` | `"hint" \| "auto" \| "manual"` | 入力への関わり方（既定 `"hint"`、後述） |
| `onOpenChange` | `(open, reason) => void` | `"auto"`: コンポジターから閉じる要求が来た |
| `closeOnEscape` | `boolean` | `"auto"`: Esc で閉じる（既定 `true`） |
| `closeOnOutsidePress` | `boolean` | `"auto"`: ポップアップと基準の外を押すと閉じる（既定 `true`） |
| `onInterestChange` | `(interested) => void` | ポインターが基準（`"hint"` 以外はポップアップも）に出入りした |
| `onAnchorPress` | `() => void` | 基準が押された |
| `placement` | `"top" \| "bottom" \| "left" \| "right"` | 基準のどちら側に出すか（既定 `"bottom"`） |
| `align` | `"start" \| "center" \| "end"` | その辺での揃え位置（既定 `"center"`） |
| `offset` | `number` | 基準からの距離（論理ピクセル、既定 `0`） |
| `collision` | `"flip" \| "none"` | `"flip"`（既定）: 出力内の余白が反対側の方が大きければそちらへ移り、辺に沿って出力内に収まるようずらす |
| `layer` | `"top" \| "window"` | `"top"`（既定）: 全ウィンドウの上。`"window"`: 自分のウィンドウのすぐ上（上にあるウィンドウには隠れる） |
| `style` | `SSDStyle` | ポップアップ自体のスタイル |

### モード

| `mode` | ポインター入力 | コンポジターが閉じる要求を出すとき |
| --- | --- | --- |
| `"hint"`（既定） | 下へ素通り | なし |
| `"auto"` | 受ける（他のウィンドウのクライアントの上でも） | 外側を押した・Esc・ウィンドウが隠れた・別の `"auto"` が開いた |
| `"manual"` | 受ける | なし |

コンポジターが `open` を直接変えることはありません。`onOpenChange(false, reason)` を
呼び、どうするかは設定側が決めます。`onOpenChange` も `trigger` もない `"auto"` には
閉じる要求は来ません（Esc もウィンドウに届きます）。`reason` は次のいずれかです。

| `reason` | いつ |
| --- | --- |
| `"outside-press"` | ポップアップ（とその中の入れ子のポップアップ）と基準の外が押された |
| `"escape"` | Esc。最後に開いた `"auto"` に届きます。開いている間 Esc はフォーカス中のウィンドウに届きません。ウィンドウに渡したいときは `closeOnEscape={false}` |
| `"anchor-gone"` | ウィンドウが隠れた（最小化・別ワークスペースへ移動） |
| `"other-popup"` | このポップアップの中に入れ子になっていない、別の `"auto"` が開いた |

### トリガー

`trigger` を付けると開閉の状態を SDK が持ちます。ブラウザの `interestfor` /
`popovertarget` に相当します。

- `"hover"`: 基準に `openDelay` ms 置くと開き、基準と（`"hint"` 以外は）ポップアップの
  両方から離れて `closeDelay` ms で閉じます。その間に戻れば開いたままです。
- `"click"`: 基準を押すたびに開閉します。

`trigger` 付きのポップアップは `onOpenChange` の要求でも閉じます（自分の
`onOpenChange` も呼ばれます）。`trigger` は最初に決めたら切り替えないでください。

### ポップアップ内のホバー

ポップアップの外では、ホバーは `onHoverChange` を持つ一番内側のノードだけに届きます。
`"auto"` / `"manual"` のポップアップの中では、DOM の `:hover` と同じく祖先の連なり
全体に届きます。中のボタン・ポップアップ・基準・基準の祖先がすべてホバー中になるので、
基準の `onHoverChange` で動かす `open={hover}` は、ポインターがメニュー上にある間
開いたままになります。

### 普通の子要素との違い

- **位置**: 大きさは中身で決まり（子要素は縦に並びます）、親のレイアウトで場所を取りません。
- **切り抜かれない**: ウィンドウの角丸や `overflow: 'hidden'` の祖先に切られません。
- **重なり順**: 専用のパスで描かれ、全ウィンドウの上（`layer="top"`）、レイヤーシェルの
  バーやオーバーレイの下に来ます。入れ子のポップアップは外側のポップアップと一緒に描かれます。
- **描画**: 普通の装飾ツリーなので、`style`・`boxShadow`・
  [ペイントシェーダー](./paint.md)・ラベル・画像・シグナルで動かす `opacity` が
  そのまま使えます。ポップアップ内の `<ShaderEffect/>` は子要素だけ描き、
  エフェクトは描きません。
- **ウィンドウのアニメーション**: ウィンドウが変形を伴うアニメーション（開閉・最小化・
  拡縮）で描かれている間は、描かれず入力も受けません。ドラッグで動かすと一緒に動きます。
- `open` は表示の切り替えだけです。閉じている間もレイアウトは保たれるので、
  開くのは軽い処理です。

### Rust

```rust
Button::new().child(
    Popup::new()
        .mode(PopupMode::Auto)
        .trigger(PopupTrigger::hover(500.0, 200.0))
        .on_open_change(|open, reason| eprintln!("open: {open} ({reason:?})"))
        .child(Label::new("最大化")),
)
```

`Popup::new().open(signal)`・`.close_on_escape(false)`・`.on_interest_change(...)`・
`.on_anchor_press(...)` は上の props に対応します。

---

## スタイルリファレンス

`style` prop は `SSDStyle` です。すべての値はシグナルにできます。長さは特記なき限り
論理ピクセルです。

### サイズ

`width`・`height`（数値または `"100%"` のような文字列）・`minWidth`・`minHeight`・
`maxWidth`・`maxHeight`・`flexGrow`・`flexShrink`。

### 余白

`gap`・`padding`・`paddingX`・`paddingY`・`paddingTop/Right/Bottom/Left`・`margin`・
`marginX`・`marginY`・`marginTop/Right/Bottom/Left`。

### レイアウトと位置

`alignItems`（`"start" | "center" | "end" | "stretch"`）・`justifyContent`
（`"start" | "center" | "end" | "space-between"`）・`position`
（`"relative" | "absolute"`）・`inset`・`top`・`right`・`bottom`・`left`・`zIndex`・
`overflow`（`"visible" | "hidden"`）・`pointerEvents`（`"auto" | "none"`）・
`transform`（`{translateX, translateY, scale, scaleX, scaleY}`）。

### 外観

`background`・`color`・`opacity`・`visible`・`cursor`・`borderRadius`。

ボーダー: `border`・`borderTop`・`borderRight`・`borderBottom`・`borderLeft`（各
`{px, color}`）に加えて `borderFit`（`"normal" | "fit-children"`）。
辺ごとの指定はその辺について `border` を上書きし、幅（コンテンツ領域もそれに合わせて
ずれます）と色の両方に効きます。0 でない幅は最低でも物理 1px になります。

影: `boxShadow` — `{x, y, blur, spread, color, inset}` 1 つ、またはその配列。CSS と
同じ意味です。[ペイントシェーダー](./paint.md#影-boxshadow)を参照。

色は `#RGB`・`#RGBA`・`#RRGGBB`・`#RRGGBBAA` で指定します。

### テキスト（`<Label/>` 用）

`fontSize`・`fontWeight`（`"normal" | "medium" | "semibold" | "bold"` または数値）・
`fontFamily`（文字列、またはフォールバックの文字列配列）・`textAlign`
（`"start" | "center" | "end"`）・`textOverflow`（既定の `"clip"`、または枠に
収まらないラベルの末尾を「…」にする `"ellipsis"`）・`lineHeight`。

```tsx
const style: SSDStyle = {
  height: 28,
  paddingX: 8,
  gap: 8,
  alignItems: 'center',
  background: window.isFocused((f) => (f ? '#1f2430cc' : '#2a2f3acc')),
  borderRadius: 8,
};
```
