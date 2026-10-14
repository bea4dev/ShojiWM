---
sidebar_position: 8.7
---

# ドラッグアンドドロップ

ウィンドウからウィンドウへ何か（テキスト、ファイル、リンク）をドラッグすると、
アプリは通常、ポインターに付いて動く小さな画像を ShojiWM に渡します。
`COMPOSITOR.dnd` を使うと、ドラッグ中に何を描くか、ドロップ後にそれをどうするかを
設定側で決められます。

組み込みのアニメーションはありません。設定側で**ドラッグビュー**を表示し、その位置・
不透明度・拡大率をシグナルで動かします。ポップイン、拒否されたドロップの「戻る」動き、
フェードなどは、普通の[アニメーション](./animations.md)のコードで書けます。ビューの
中身はウィンドウ装飾と同じツリーで、[paint シェーダー](./paint.md)や
[`<ShaderEffect>`](./effects.md) も使えます。

```tsx
import {Box, COMPOSITOR, DragIcon, Label} from 'shoji_wm';

COMPOSITOR.dnd.event.onStart((drag) => {
  const view = drag.show({
    content: () => (
      <Box direction="column" style={{alignItems: 'center', gap: 4}}>
        <DragIcon />
        <Label text={drag.action.value === 'copy' ? '+ Copy' : ''} />
      </Box>
    ),
  });
  drag.onEnd(() => view.dispose());
});
```

[デフォルト設定](./default-config.md)はこの API で、すりガラスのカードに操作のバッジと
ドラッグ中のデータの種類を示すラベルを付けて表示し、どこにも受け入れられなかった
ドロップはドラッグ開始位置へ戻します。ソースは `packages/config/src/drag-and-drop.tsx`
にあり、自作する際の出発点にちょうど良いはずです。

## リスナーが無い場合

`onStart` のリスナーが1つも登録されていなければ、ShojiWM はほかのコンポジターと同様に
アプリの画像をポインターの位置に描きます。リスナーを1つでも登録すると、ShojiWM は
リスナーが表示したビュー**だけ**を描きます。何も表示しないリスナーを登録すると、画像は
表示されなくなります。

## ドラッグセッション

`COMPOSITOR.dnd.event.onStart(listener)` は、ドラッグが始まると `DragSession` を渡して
`listener` を呼びます。戻り値はリスナーを解除する関数です。`COMPOSITOR.dnd.current` は
進行中のドラッグ（無ければ `null`）を持つシグナルです。

| メンバー | 型 | 意味 |
| --- | --- | --- |
| `id` | `number` | ドラッグごとに一意 |
| `mimeTypes` | `readonly string[]` | データが提供される形式。データの中身は公開されません |
| `actions` | `{copy, move, ask}` | ソースが許す操作 |
| `action` | `ReadonlySignal<DragAction>` | 今ドロップした場合の操作: `"copy"`、`"move"`、`"ask"`、`"none"` |
| `target` | `ReadonlySignal<WaylandWindow \| null>` | ポインターの下のウィンドウ |
| `sourceWindow` | `WaylandWindow \| null` | ドラッグ元のウィンドウ |
| `device` | `"pointer" \| "touch"` | 何でドラッグしているか |
| `origin` | `{x, y}` | ドラッグ開始時のポインター位置（グローバル論理座標） |
| `icon` | `ReadonlySignal<DragIconInfo \| null>` | アプリの画像。`width`、`height` と `offsetX` / `offsetY`（ポインターから見た左上の位置）。無ければ `null` |
| `ended` | `ReadonlySignal<DragEnd \| null>` | ドラッグが終わるまで `null` |
| `animation` | `AnimationController` | このドラッグのアニメーション（`window.animation` と同じ） |
| `show(options)` | `DragView` | このドラッグのビューを描く（[後述](#ドラッグビュー)） |
| `onEnd(listener)` | `() => void` | 終了時に `DragEnd` を渡して1回呼ばれる。解除関数を返す |

`action` は、何もない場所の上でも、データを拒否するウィンドウの上でも `"none"` です。
区別するには `target` を見ます。

```ts
const refused = drag.action.value === 'none' && drag.target.value !== null;
```

データの種類は `mimeTypes` から分かります。例:

| 形式 | たいていは |
| --- | --- |
| `text/uri-list` | ファイルマネージャーからのファイル |
| `text/x-moz-url`、`_NETSCAPE_URL` | ブラウザーからのリンク |
| `image/*` | 画像 |
| `text/plain`、`UTF8_STRING` | テキスト |

### ドラッグの終わり方

`onEnd` に渡され、`ended` が持つ `DragEnd`:

| フィールド | 意味 |
| --- | --- |
| `kind` | `"dropped"`（ボタンが離された）または `"cancelled"`（アプリが中断した） |
| `accepted` | ターゲットがデータを受け取れば `true`。何もない所へのドロップや、拒否されたドロップは `false` |
| `target` | ドロップ先のウィンドウ（無ければ `null`） |
| `position` | ドラッグ終了時のポインター位置（グローバル論理座標） |

## ドラッグビュー

`drag.show(options)` はビューを描き、`DragView` を返します。ビューは
`view.dispose()` を呼ぶまで、**ドラッグが終わった後も**残るので、ドロップの
アニメーションを付けられます。`show` を複数回呼べば複数のビューを描けます。

| オプション | 型 | 既定 | 意味 |
| --- | --- | --- | --- |
| `anchor` | `"pointer" \| "global"` | `"pointer"` | `x` / `y` の基準 |
| `x`, `y` | `number` | `0` | `"pointer"` ならポインターからのずれ。`"global"` ならグローバル論理座標 |
| `opacity` | `number` | `1` | ビュー全体の不透明度（`0`〜`1`） |
| `scale` | `number` | `1` | アンカー点を中心とした拡大率 |
| `content` | ツリー または `() => ツリー` | アイコンのみ | 描く内容（[後述](#中身)） |
| `maxLinger` | `number`（ミリ秒） | `5000` | `dispose()` が来ない場合に、ドラッグ終了後もビューが残れる時間 |

`content` と `maxLinger` 以外はすべてシグナル（や `computed`）にでき、ビューはその値に
追従します。

`DragView` には `dispose()`、`disposed`、ビューが消えると解決する Promise の `closed`
があります。

### アンカー点

ビューは**アンカー点**、つまりビューの中でポインターが当たる点を基準に配置されます。
中身に `<DragIcon/>` があれば、アプリの画像のうちポインターが当たっている点がアンカー点
です。そのため `anchor: "pointer"` かつ `x = y = 0` なら、画像はアプリが指定した通りの
位置でポインターの下に来て、中身の残りはその周りに並びます。`<DragIcon/>` が無ければ、
アンカー点は中身の左上です。

`scale` もこの点を中心に拡大縮小するので、縮むビューはポインターに向かって縮みます。

`anchor: "pointer"` の間は、ShojiWM がビューをポインターに合わせて動かします。マウスを
動かしても設定側のコードは一切走りません。ビューを追従させたくなくなったら（たいていは
ドラッグ終了後）、`"global"` に切り替えます。

```ts
const view = drag.show({
  anchor: computed(() => (drag.ended.value ? 'global' : 'pointer')),
  x: computed(() => drag.ended.value?.position.x ?? 0),
  y: computed(() => drag.ended.value?.position.y ?? 0),
});
```

### 中身

`content` は[ウィンドウ装飾](./components.md)と同じコンポーネント（`Box`、`Label`、
`AppIcon`、`Image`、paint シェーダー、`<ShaderEffect>`）のツリーです。アプリの画像を置く
場所に `<DragIcon/>` を書きます。アプリが画像を渡さなかったときは、その子要素が
**代わりに**描かれます。

```tsx
<DragIcon>
  <Label text="📄" />
</DragIcon>
```

画像が入るのは最初の `<DragIcon/>` だけです。`<AppIcon/>` はドラッグ元のウィンドウの
アイコンを表示します。

`content` を関数にすると、その中で読んだシグナルが変わるたびに再実行されます。
ここではあまり頻繁に変わらない状態（`drag.action`、`drag.target`、`drag.icon`）を
読んでください。再実行のたびにレイアウトと描画をやり直すので、アニメーションは
`x`・`y`・`opacity`・`scale` で動かします。こちらは描き終えた絵を動かすだけです。

`backdropSource()` を使う `<ShaderEffect>` には、ビューの背後（ウィンドウと、
ビュー自身のうちエフェクトより下の部分）が映ります。デフォルト設定はこれでカードの
背後をぼかしています。

```tsx
const blur = compileEffect({
  input: backdropSource(),
  capturePadding: 24,
  invalidate: {kind: 'on-source-damage-box', damagePadding: 8},
  alpha: 'preserve',
  pipeline: [dualKawaseBlur({radius: 4, passes: 2})],
});

<Box style={{borderRadius: 10, overflow: 'hidden'}}>
  <ShaderEffect shader={blur}>
    <Box style={{background: '#1f243099', padding: 6}}>
      <DragIcon />
    </Box>
  </ShaderEffect>
</Box>
```

制限:

- **`opacity` を 1 未満にするとエフェクトも薄くなり**、ブラー越しに背後が透けます。
  ビューを不透明に見せたい間は `opacity` を 1 のままにし、薄くするのは出入りの
  フェードだけにしてください。
- **`scale` が 1 以外の間は**、エフェクトに映る内容が実際の背後と少しずれます。短い
  ポップインや縮小の間ならほとんど目立ちません。
- ビューはクリックできません（ポインターはドラッグ中です）。`onClick` などのハンドラーは
  無視され、`windowSource()` には何も映りません。

## ドロップのアニメーション

アニメーションには `window.animation` と同じ使い方の `drag.animation` を使います。
`start()` は、目標値に達すると `true`（中断されると `false`）で解決する Promise を返す
ので、アニメーション後のビュー破棄は1行で書けます。

次のビューはポップインしてポインターに付いて動き、ドラッグ終了時には、受け入れられれば
その場でフェードし、拒否されればドラッグ開始位置へ戻ります。

```tsx
import {
  COMPOSITOR, DragIcon, animationVariable, computed,
} from 'shoji_wm';

const APPEAR = animationVariable('drag-appear');
const FINISH = animationVariable('drag-finish');

COMPOSITOR.dnd.event.onStart((drag) => {
  const appear = drag.animation.variable(APPEAR);
  const finish = drag.animation.variable(FINISH);
  drag.animation.start(APPEAR, {from: 0, to: 1, duration: 200});

  const at = (axis: 'x' | 'y') =>
    computed(() => {
      const end = drag.ended.value;
      if (!end) return 0; // ポインターに追従中
      const from = end.position[axis];
      return end.accepted ? from : from + (drag.origin[axis] - from) * finish.value;
    });

  const view = drag.show({
    anchor: computed(() => (drag.ended.value ? 'global' : 'pointer')),
    x: at('x'),
    y: at('y'),
    opacity: computed(() => (drag.ended.value ? 1 - finish.value : appear.value)),
    scale: computed(() => 0.85 + 0.15 * appear.value),
    content: <DragIcon />,
  });

  drag.onEnd((end) => {
    drag.animation
      .start(FINISH, {from: 0, to: 1, duration: end.accepted ? 150 : 400})
      .then(() => view.dispose());
  });
});
```

## ドラッグ中にカーソルを変える

[`COMPOSITOR.cursor.overrideKind`](./cursor.md#カーソルの形を上書きする) を使うと、
ポインターの下のウィンドウの指定に関係なく、好きなカーソルの形を表示できます。たとえば、
ポインターの下のウィンドウがデータを拒否している間だけ「ドロップ不可」カーソルにするには:

```ts
import {COMPOSITOR, effect} from 'shoji_wm';

COMPOSITOR.dnd.event.onStart((drag) => {
  const stop = effect(() => {
    const refused = drag.action.value === 'none' && drag.target.value !== null;
    COMPOSITOR.cursor.overrideKind(refused ? 'no-drop' : null);
  });
  drag.onEnd(() => {
    stop();
    COMPOSITOR.cursor.overrideKind(null);
  });
});
```

ドラッグが終わったら、`overrideKind(null)` でカーソルを戻すのを忘れないでください。
