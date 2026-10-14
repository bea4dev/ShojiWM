---
sidebar_position: 11
---

# アニメーション

ShojiWM にはアニメーションの実現方法が**2つ**あり、それぞれ得意分野が異なります。

| 方法 | 実行される場所 | 得意なこと |
| --- | --- | --- |
| **シグナル駆動**（`window.animation` ＋ `animationVariable`） | TypeScript ランタイム | 装飾・クローム：ホバー、フォーカス、GPU トランスフォーム、任意の TSX にマップするもの |
| **コンポジター駆動**（`window.scheduleAnimation(...)`） | Rust コア | マネージドウィンドウのジオメトリ／不透明度：開く・閉じる・最小化・移動・リサイズ・ワークスペース切り替え |

一言で違いを言うと、**シグナル**方式では*あなたが*各フレームの値を TS で計算して
合成が再実行されますが、**`scheduleAnimation`** ではアニメーション全体を一度記述して
**Rust が再生**し、その結果をレイアウトの上に差分として適用します――フレームごとの
TS 処理がありません。

指針は末尾の [どちらを使うべきか？](#どちらを使うべきか) を参照してください。

---

## シグナル駆動のアニメーション

これは**アニメーション変数**――時間とともに値がなめらかに補間される名前付きトークン――で
駆動します。変数をシグナルとして読み、トランスフォーム・不透明度・その他のスタイルに
流し込みます。開始／停止はイベントハンドラから行います。各ウィンドウは変数ごとに自身の
進捗を保持します。

### アニメーション変数

モジュールスコープで `animationVariable(debugName?)` を使ってトークンを一度作成し、
`window.animation` を通じて使います。

```ts
import {animationVariable, milliseconds, seconds} from 'shoji_wm';

const open = animationVariable('open');

COMPOSITOR.event.onOpen((window) => {
  window.animation.start(open, {duration: seconds(0.18), from: 0, to: 1});
});

COMPOSITOR.event.onFocus((window, focused) => {
  window.animation.start(open, {duration: milliseconds(120), to: focused ? 1 : 0});
});
```

`milliseconds(n)` と `seconds(n)` は可読性のためのヘルパーで、どちらもミリ秒の数値を
返すだけです。

### アニメーションコントローラ

`window.animation`（`AnimationController`）は以下を公開します。

| メソッド | 目的 |
| --- | --- |
| `variable(v)` | 変数の進捗を `ReadonlySignal<number>` として読む |
| `signal(v)` | `variable` の別名 |
| `start(v, options)` | アニメーションを開始／再開 |
| `stop(v)` | 現在値を保ったまま停止 |
| `set(v, value)` | 実行中のタスクをキャンセルして値に即時ジャンプ |
| `running(v)` | アニメーション実行中なら `true` |

`start` のオプション（`AnimationStartOptions`）:

| オプション | 型 | 意味 |
| --- | --- | --- |
| `duration` | `number`（ミリ秒） | 全体の時間 |
| `from` | `number` | 開始値（省略時は現在値――なめらかな再ターゲット） |
| `to` | `number` | 目標値（デフォルト `1`） |
| `easing` | `(t: number) => number` | `0..1` の進捗に適用するイージング |
| `repeat` | `"loop" \| "ping-pong"` | 繰り返しの挙動 |

`from` を省略すると方向転換や再ターゲットがなめらかになります――アニメーションは現在の
値から続きます。

`start` は `Promise<boolean>` を返します。値が `to` に達すると `true`、`start`・`stop`・`set`
で中断されると `false` で解決します（繰り返しアニメーションは中断でのみ終わります）。
アニメーション後の後片付けに使えます。

```ts
drag.animation.start(fade, {to: 0, duration: 150}).then((done) => {
  if (done) view.dispose();
});
```

### 合成内で変数を読む

`variable(v)` は、スタイルにマップできるシグナルを返します。合成内で読むと、
アニメーションが進む各フレームで装飾が更新されます。

```tsx
COMPOSITOR.window.composition = (window) => {
  const t = window.animation.variable(open);
  const scale = t((x) => 0.8 + x * 0.2); // 0.8 → 1.0
  window.transform.scaleX = scale;
  window.transform.scaleY = scale;
  window.transform.opacity = t;
  return (/* … */);
};
```

値が合成の読むシグナルにあるため、各フレームで（対象を絞った）再評価が走ります。その
柔軟さこそが利点ですが、同時にこの経路はフレームごとに TS 処理のコストがかかるので、
下記の重いジオメトリアニメーションではなく、クローム用途にとどめてください。

---

## コンポジター駆動のアニメーション: `scheduleAnimation`

`window.scheduleAnimation(options)` は、完全なアニメーションの記述を Rust コアに渡します。
Rust は**毎フレーム自分で**補間し、その結果をマネージドウィンドウに適用します。TS
ランタイムはフレームごとには関与しません――再合成もフレームごとの IPC もありません――ので、
頻繁で重い遷移（開く・閉じる・最小化・移動・リサイズ・ワークスペース）に適した軽量パスです。

```ts
window.scheduleAnimation({
  channel: 'open',
  rect: {
    from: {x: 0, y: 200, width: 0, height: 0},
    to:   {x: 0, y: 0,   width: 0, height: 0},
    duration: 500,
    easing: {kind: 'cubicBezier', x1: 0.2, y1: 0, x2: 0, y2: 1},
    mode: 'add',
  },
  opacity: {from: 0, to: 1, duration: 500, mode: 'multiply'},
});
```

### アニメーションできる対象

`ManagedWindowScheduleAnimationOptions` は最大3つの独立したプロパティと、チャンネルを
持ちます。

| フィールド | アニメーション対象 | オプション型 |
| --- | --- | --- |
| `rect` | ウィンドウの `{x, y, width, height}` | `ManagedWindowRectAnimationOptions` |
| `offset` | 位置の `{x, y}` オフセット | `ManagedWindowPointAnimationOptions` |
| `opacity` | スカラーの不透明度 | `ManagedWindowScalarAnimationOptions` |
| `channel` | *(文字列)* アニメーションをグループ化 — [チャンネル](#チャンネルとキャンセル) 参照 | — |

`rect` / `offset` / `opacity` はいずれも同じ形を取ります。

| オプション | 型 | 意味 |
| --- | --- | --- |
| `to` | 値 | 目標（必須） |
| `from` | 値 | 開始（任意――省略時は現在値） |
| `duration` | `number`（ミリ秒） | 全体の時間 |
| `easing` | イージング | [イージング](#イージング) 参照（デフォルトは linear） |
| `mode` | `"override" \| "add" \| "sub" \| "multiply"` | ベースとの組み合わせ方（下記） |

`rect` と `offset` の値は `{x, y, …}`、`opacity` は数値です。`rect`／`offset` の `mode` に
`"multiply"` は**使えません**。

### モード: アニメーションがレイアウトとどう組み合わさるか

これが `scheduleAnimation` の肝です。アニメーションの値はウィンドウの状態を単純に
置き換えるのではなく、あなたのウィンドウマネージャがライブで計算しているベース値と
`mode` に従って**組み合わされます**。

| モード | 結果 |
| --- | --- |
| `"override"` | `animated` — ベース値を置き換える |
| `"add"` | `base + animated` — レイアウトの上に差分を加える |
| `"sub"` | `base - animated` |
| `"multiply"` | `base × animated`（不透明度のみ） |

`add` こそが、これらのアニメーションを**レイアウトに追従させる**仕組みです。上の open の
例では、`rect.mode: 'add'` が `+200px` の縦オフセットを `0` に向けて減衰させます――つまり
ウィンドウは、*タイル／フローティングのレイアウトが現在置く位置に対して相対的に*
スライドして所定位置に収まります。アニメーション途中でレイアウトが動いても（別の
ウィンドウが開く、タイルがリサイズされる）、Rust が毎フレーム、ライブのベース矩形に
アニメーション差分を加えるため、スライドは正しく着地します。不透明度の `multiply` は、
ベースの不透明度が変化中のウィンドウともフェードが噛み合うようにします。

### イージング

`easing` は以下を受け付けます。

- `"linear"`（デフォルト）または `{kind: "linear"}`
- `{kind: "cubicBezier", x1, y1, x2, y2}` — CSS 風の三次ベジェ曲線
- `EasingFunction` の値

```ts
easing: {kind: 'cubicBezier', x1: 0.2, y1: 0, x2: 0, y2: 1}
```

### チャンネルとキャンセル

`channel` はアニメーションに名前を付けるもので、独立したアニメーションを同時に走らせたり、
個別に対象指定したりできます。

- **同じ**チャンネルで再スケジュールすると、そのチャンネルのアニメーションを置き換えます。
- `window.cancelAnimation(channel)` はそのチャンネルだけをキャンセルします。
- `window.cancelAnimation()`（引数なし）は**すべて**のチャンネルをキャンセルします。

デフォルト設定は、開く／閉じる・最小化・ワークスペース切り替えの演出に別々のチャンネルを
使っています。そのため、たとえばウィンドウが開くアニメーションを再生し終える前に
ワークスペースを切り替えても、開くアニメーションが中断されません。

```ts
const OPEN = 'open';
const WORKSPACE = 'workspace-visual';

window.scheduleAnimation({channel: OPEN, /* … */});
window.scheduleAnimation({channel: WORKSPACE, /* … */}); // OPEN と並行して実行
window.cancelAnimation(WORKSPACE);                        // WORKSPACE だけをキャンセル
```

### 実例: 開く・閉じる

デフォルトのウィンドウマネージャより。`rect` は `add`（減衰するオフセット）を、`opacity` は
`multiply`（ベース不透明度と噛み合うフェード）を使っている点に注目してください。

```ts
function scheduleOpenAnimation(window) {
  window.scheduleAnimation({
    channel: 'open',
    rect: {
      from: {x: 0, y: 200, width: 0, height: 0},
      to:   {x: 0, y: 0,   width: 0, height: 0},
      duration: 500, easing: WINDOW_OPEN_EASING, mode: 'add',
    },
    opacity: {from: 0, to: 1, duration: 500, easing: WINDOW_OPEN_EASING, mode: 'multiply'},
  });
}

function scheduleCloseAnimation(window) {
  window.setCloseAnimationDuration(500); // フェードのためサーフェスを生かしておく
  window.scheduleAnimation({
    channel: 'close',
    rect: {
      from: {x: 0, y: 0, width: 0, height: 0},
      to:   {x: 0, y: 120, width: 0, height: 0},
      duration: 500, easing: WINDOW_CLOSE_EASING, mode: 'add',
    },
    opacity: {from: 1, to: 0, duration: 500, easing: WINDOW_CLOSE_EASING, mode: 'multiply'},
  });
}
```

閉じるアニメーションでは、`scheduleAnimation` を `window.setCloseAnimationDuration(ms)` と
組み合わせて、コンポジターがサーフェスを破棄する前にアニメーションを再生し切るだけの
時間、生かしておくようにします。

---

## どちらを使うべきか？

| こちらを使う… | こんなとき |
| --- | --- |
| **`scheduleAnimation`** | マネージドウィンドウの位置・サイズ・不透明度をアニメーションするとき――開く・閉じる・最小化・移動・リサイズ・ワークスペース遷移。軽量パス（Rust が補間、フレームごとの TS なし）で、`add` モードはライブのレイアウト変化ときれいに合成されます。 |
| **シグナル駆動の `window.animation`** | TSX で組み立てる装飾をアニメーションするとき――タイトルバーの色、ホバー／フォーカスのフィードバック、任意のロジックから導く GPU の `transform`／`opacity`。完全な柔軟性が得られますが、フレームごとの TS 再評価のコストがかかります。 |

両者は組み合わせられます。ウィンドウの登場は `scheduleAnimation` で駆動しつつ、ボーダーの
フォーカスグローはシグナル変数で駆動する、といった使い方が可能です。

シグナル駆動のアニメーションは、ウィンドウが属する出力の時計で、その出力のフレームごとに
進みます。自分でフレームごとの処理やタイマーを書く場合は
[フレームのタイミングと poll](./timing.md) を参照してください。
