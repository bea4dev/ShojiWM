---
sidebar_position: 7
---

# ウィンドウの合成

`COMPOSITOR.window.composition` は ShojiWM のカスタマイズの心臓部です。ウィンドウを
受け取り、それをどう配置・装飾するかを表す TSX ツリーを返す関数を割り当てます。
コンポジターはすべてのトップレベルウィンドウに対してこれを呼び、読み取った値が
変化するたびに（差分的に）再実行します。

```tsx
COMPOSITOR.window.composition = (window) => (
  <ManagedWindow rect={window.position} zIndex={1}>
    <WindowBorder
      style={{
        borderRadius: 10,
        border: {
          px: 2,
          color: window.isFocused((f) => (f ? "#d7ba7d" : "#4f5666")),
        },
      }}
    >
      <Box direction="column">
        <Box
          direction="row"
          style={{ height: 28, paddingX: 8, gap: 8, alignItems: "center" }}
        >
          <AppIcon icon={window.icon} style={{ width: 16, height: 16 }} />
          <Label text={window.title} style={{ flexGrow: 1, fontSize: 13 }} />
        </Box>
        <ClientWindow />
      </Box>
    </WindowBorder>
  </ManagedWindow>
);
```

ツリーには必ず、ちょうど1つの [`<ManagedWindow/>`](#managedwindow) が、ちょうど1つの
[`<ClientWindow/>`](#clientwindow) を包む形で含まれている必要があります。その間にある
もの――ボーダー・タイトルバー・ボタン――があなたの装飾で、[SSD
コンポーネント](./components.md) で組み立てます。

## `window` オブジェクト

引数は `WaylandWindow`、つまり1つのウィンドウへのライブでリアクティブなハンドルです。
合成内でそのシグナルを読むと、変更に自動的に購読されます。

### リアクティブなプロパティ

それぞれ `ReadonlySignal` です――`window.title()` や `window.title.value` のように
読むか、`window.isFocused((f) => f ? 'a' : 'b')` のようにマップします。

| プロパティ        | 型                        | 意味                                              |
| ----------------- | ------------------------- | ------------------------------------------------- |
| `title`           | `string`                  | ウィンドウタイトル                                |
| `appId`           | `string \| undefined`     | アプリケーション id（例: `"org.gnome.Nautilus"`） |
| `icon`            | `WindowIcon \| undefined` | アプリケーションアイコン                          |
| `isFocused`       | `boolean`                 | キーボードフォーカスを持つ                        |
| `isFloating`      | `boolean`                 | フローティング（非タイル）                        |
| `isMaximized`     | `boolean`                 | 最大化                                            |
| `isFullscreen`    | `boolean`                 | フルスクリーン                                    |
| `decoration`      | `WindowDecorationState`   | 有効な CSD／SSD ネゴシエーション状態              |
| `isResizable`     | `boolean`                 | クライアントがインタラクティブリサイズを許可      |
| `isTransient`     | `boolean`                 | 別ウィンドウの子（ダイアログ）                    |
| `parentId`        | `string \| undefined`     | トランジェントの場合の親ウィンドウ id             |
| `sizeConstraints` | `WindowSizeConstraints`   | クライアントの最小／最大サイズ                    |
| `interaction`     | スナップショット          | 現在のポインター／ドラッグの状態                  |

非リアクティブなヘルパー: `id`（安定した文字列）、`position` / `rect`（現在の論理
ジオメトリ）、`state`（ウィンドウごとのストア。[状態とシグナル](./state-and-signals.md)
を参照）、`transform`（GPU トランスフォーム）、`animation`
（[アニメーション](./animations.md) を参照）。

### クライアント側／サーバー側装飾

Wayland アプリは、自分でタイトルバーとボーダーを描く CSD（Client-Side
Decoration）か、コンポジターに描画を任せる SSD（Server-Side Decoration）を
利用します。ShojiWM の方針を一度設定し、ネゴシエーション結果を composition で
読み取ります。

```tsx
COMPOSITOR.window.decoration.configure((window, context) => {
  const appId = (window.appId() ?? "").toLowerCase();

  // アプリを識別できるまでは CSD の初期値を維持します。
  if (appId.length === 0) {
    return { mode: context.clientPreference ?? "client" };
  }
  if (appId.includes("firefox")) {
    return { mode: "client" };
  }
  return { mode: "server" };
});

COMPOSITOR.window.composition = (window) => {
  const decoration = window.decoration();
  const useClientDecoration =
    decoration.mode === "client" &&
    !(
      decoration.clientPreference === "server" &&
      decoration.configuredMode === "server"
    );

  return (
    <ManagedWindow rect={window.position} zIndex={1}>
      {useClientDecoration ? (
        <ClientWindow />
      ) : (
        <WindowBorder style={{ border: { px: 2, color: "#4f5666" } }}>
          <Box direction="column">
            {/* タイトルバー */}
            <ClientWindow />
          </Box>
        </WindowBorder>
      )}
    </ManagedWindow>
  );
};
```

resolver は装飾オブジェクトの作成、クライアント要求の変更、関連メタデータの変更、
TS config のリロード時に同期的に実行されます。`context` の内容は次の通りです。

旧 KDE decoration manager のグローバル初期値は、ウィンドウ単位のメタデータより先に
送られるため、CSD に設定されています。最初の装飾要求時点では app id が空の場合があるので、
上の例のように app id が空の間は CSD の初期値を維持し、メタデータ到着後にアプリごとの
最終方針を適用してください。早い段階で SSD を返すと、Firefox や Chromium が簡素な
ウィンドウ枠を構築し、後から CSD を返しても完全には作り直さないことがあります。resolver は
副作用なしである必要があり、この中で `focus()` などのウィンドウ操作を呼ぶとエラーになります。

| プロパティ         | 意味                                                                        |
| ------------------ | --------------------------------------------------------------------------- |
| `protocol`         | `xdg-decoration-v1`、`kde-server-decoration`、`xwayland`、`none` のいずれか |
| `clientPreference` | クライアントが要求したモード。未指定なら `null`                             |
| `canNegotiate`     | 装飾プロトコルを通して決定をクライアントへ送れるか                          |
| `reason`           | ポリシーが評価された理由                                                    |

`window.decoration.configuredMode` は ShojiWM が最後に選択したモードです。
`window.decoration.mode` はクライアントが ack し commit した有効なモードなので、描画の
通常の基準にはこちらを使います。XDG の configure／ack／commit 中は、両者が一時的に
異なる場合があります。`clientPreference` と `configuredMode` がどちらも `server` なら、
クライアントとコンポジターはすでに SSD で合意しているため、上の例では effective mode の
反映を待たず SSD を描画します。これにより、activation まで configure の commit を遅らせる
ツールキットでも起動直後に装飾なしの状態にならず、CSD を要求したクライアントへ SSD を
強制することもありません。

`canNegotiate` が `false` の場合でも、ShojiWM はどちらの composition を描くか選べます。
ただし、クライアントに CSD の追加や削除を強制することはできません。XWayland は
`protocol: 'xwayland'` として確認できますが、これらの Wayland 装飾プロトコルでは
ネゴシエーションされません。

旧 KDE プロトコルでは、クライアントとコンポジターの再交渉ループを防ぐため、ShojiWM は
同じackの繰り返しを抑止します。同じ要求が8回到着すると、そのループについて一度だけ
警告を通常ログへ記録します。要求ごとに警告を出し続けることはありません。XDG decoration
の要求には、プロトコルで必須の configure 応答を毎回返します。

### メソッド

| メソッド                          | 効果                                               |
| --------------------------------- | -------------------------------------------------- |
| `close()`                         | クライアントに閉じるよう要求                       |
| `maximize()` / `unmaximize()`     | 最大化の切り替え                                   |
| `minimize()`                      | 最小化                                             |
| `fullscreen()` / `unfullscreen()` | フルスクリーンの切り替え                           |
| `focus()`                         | キーボードフォーカスを与え前面に出す               |
| `scheduleAnimation(options)`      | マネージドウィンドウのジオメトリをアニメーション   |
| `cancelAnimation(channel?)`       | 実行中のアニメーションをキャンセル                 |
| `setCloseAnimationDuration(ms)`   | 閉じるアニメーションに合わせてサーフェス破棄を遅延 |
| `isXWayland()`                    | XWayland 上で動作中なら `true`                     |

## ManagedWindow

`<ManagedWindow/>` はウィンドウをレイアウトシステムに結びつけるアンカーです。
ウィンドウごとに1つ置きます。

| Prop             | 型                       | 意味                                                                       |
| ---------------- | ------------------------ | -------------------------------------------------------------------------- |
| `rect`           | `ManagedWindowRect`      | ウィンドウの論理的な `{x, y, width, height}`                               |
| `zIndex`         | `number`                 | 重なり順（大きいほど上）                                                   |
| `workspace`      | `string \| number`       | ワークスペース割り当て                                                     |
| `visibleOutputs` | `string[] \| null`       | 指定出力に限定（`null` で全出力）                                          |
| `visible`        | `boolean`                | アンマップせずに表示／非表示                                               |
| `idle`           | `boolean`                | フォーカス巡回から除外。背景として扱う                                     |
| `interactive`    | `boolean`                | `false` のときポインター入力を無視                                         |
| `forceRectSize`  | `boolean`                | クライアントを `rect` のサイズに強制                                       |
| `tiled`          | `boolean`                | タイル状態をクライアントに送る                                             |
| `opacity`        | `number`                 | `0.0`〜`1.0`                                                               |
| `transform`      | `ManagedWindowTransform` | 追加の GPU トランスフォーム                                                |
| `allowTearing`   | `boolean`                | フルスクリーン＋ダイレクトスキャンアウト時のテアリングを許可（ゲーム向け） |

すべての prop はリアクティブなレイアウトのためにシグナルを受け付けます。`rect`・
`zIndex` などは通常、あなたのウィンドウマネージャのロジックが駆動します。

## ClientWindow

`<ClientWindow/>` はクライアントの実際のサーフェスバッファを描画します。リーフノードで
子要素は持ちません。別名: `<Window/>`。

```tsx
<ClientWindow />
```

素の ClientWindow は、`xdg_surface.window_geometry` の外側にある CSD の影や透明な
リサイズマージンを含め、クライアント所有のサーフェスツリー全体を保持します。
ManagedWindow のスロットを占有しているという理由だけでは切り取られません。

クリップを所有するのは周囲の SSD 階層です。`border` を持つコンテナで囲むと、子要素は
そのボーダーの内側（角丸を含む）に切り取られます。明示的に切り取る場合は
`overflow: "hidden"`、border があっても切り取りたくない場合は
`overflow: "visible"` を指定します。

```tsx
<WindowBorder
  style={{ border: { px: 2, color: borderColor }, borderRadius: 8 }}
>
  <ClientWindow />
</WindowBorder>
```

:::tip[フルスクリーンのファストパス]
フルスクリーンのウィンドウでは、`<ManagedWindow/>` の中に**素の `<ClientWindow/>`
だけ**を返します（ボーダーもタイトルバーもなし）。他に何も描画しないことで、TTY
バックエンドがクライアントバッファをプライマリプレーンに昇格（ダイレクトスキャンアウト）
でき、最小のレイテンシになります。デフォルト設定はまさにこれを行っています。
:::
