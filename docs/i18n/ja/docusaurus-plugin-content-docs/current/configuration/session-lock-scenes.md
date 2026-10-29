---
sidebar_position: 12
---

# ロック背景と解除オーバーレイ

セッションがロックされている間、ShojiWM は通常、ロッカーの `ext-session-lock-v1`
サーフェスしか描きません。この API を使うと、設定で選んだウィンドウやレイヤーシェルの
サーフェスを、透明なロック画面の**後ろ**に表示し続けられます（動く壁紙やスクリーンセーバー
など）。また、ロック解除に成功したあと、アニメーション付きのオーバーレイでロック画面から
デスクトップへ切り替えることもできます。

この API はロッカーではなく、パスワード入力や認証は行いません。何も選ばなければ、
ロックの動作は今までと同じです。

## 背景の選択

```ts
const locked = COMPOSITOR.session.configureLockBackground({
  windowIds: [wallpaperWindow.id],
  layerIds: [wallpaperLayer.id],
});
```

- 引数には現在の `WaylandWindow.id` / `WaylandLayer.id` を渡します。設定の中で
  （たとえば `COMPOSITOR.event.onOpen` で）目的のクライアントを見つけて選び、
  そのインスタンスが変わったら呼び直してください。空の配列を渡すと背景を消します。
- ID は最大 64 個までです。それぞれ空でなく、512 バイト以下で、制御文字を含まない必要が
  あります。知らないフィールドはエラーになります。
- 戻り値は「今ロックされているかどうか」です。この呼び出し自体がロックや解除を
  することはありません。
- 選択はいつでも（ロック中でも）変えられ、次のフレームから反映されます。モジュールの
  トップレベルではなく、設定の読み込み後（イベント・タイマー・`onEnable`）に呼んでください。

描かれるもの:

- 選んだルートサーフェスのツリーと、そのクライアントのサブサーフェスだけです。別管理の
  ポップアップ、SSD、エフェクト、選ばれていないウィンドウやレイヤーは表示されません。
  ウィンドウはクライアント矩形の位置に描かれ、ウィンドウの composition は使われません。
- 重なり順は通常のウィンドウ・レイヤーの順に従いますが、選んだウィンドウは常に選んだ
  レイヤーより上になります。
- 入力は変わりません。キーボードとポインターはロッカーにしか届きません。

選択が無効になるとき:

- 選択は設定ランタイムに属します。ランタイムが停止・リロードすると外れるので、
  必要なら `onEnable` で選び直してください。
- 出力にバッファ付きのロックサーフェスが現れるまでは何も描きません。ロッカーが落ちると、
  その出力は背景も含めて空のロック画面に戻ります。

:::warning
設定は信頼される前提です。クライアントを選ぶと、そのサーフェスツリーに描かれた UI も
含めて、ロック中にその画素が見えるようになります。個人情報を表示しうるアプリではなく、
背景専用のクライアントを選んでください。これはサンドボックスではありません。
:::

## 解除時の受け渡し

`COMPOSITOR.effect.overlay` は `placement: "unlock"` を受け付けます。ロック中に作れる
唯一のオーバーレイで、ロッカーが解除したあともデスクトップの上に残ります。作成には
次の条件があります。

- ロック中であること
- 入力が `snapshotSource()` であること
- `persistent: false` であること
- `maxDuration` が 3000 ms 以下であること

ほかの placement はロックされると閉じられ、ロック中は作れません。新しくロックされると
それまでのオーバーレイは閉じられます。セッションの一時停止（VT 切り替え）と電源オフの
出力でもオーバーレイは閉じられます。有効なロックサーフェスが無い間、解除オーバーレイは
描かれません。

スナップショットに入るのはロック画面の内容（ロックサーフェスと選んだ背景。カーソルは
含まない）です。どちらにも覆われていない部分は**透明**になるため、解除した瞬間から
その部分には実際のデスクトップが透けて見えます。背景で出力全体を覆うか、シェーダーで
不透明な色を返してください（例: `vec4(frozen.rgb, 1.0)`）。

受け渡しは**認証に成功してから**始めてください。

1. まだロックされている間に、切り離したタスクからオーバーレイを作ります。コンポジタが
   完了を待つコールバックから await しないでください（[出力オーバーレイ](./output-overlays.md)
   を参照）。
2. オーバーレイの準備ができる（Promise が解決する）まで待ちます。これでスナップショットが
   撮られています。
3. ロッカーの通常の認証済みの経路で解除します。
4. 実際のデスクトップの上で、シェーダーの uniform を動かします。
5. 終わったらハンドルを dispose します。失敗に備えて、`closed` Promise・ネイティブの
   期限・自前のタイムアウトを使ってください。

```ts
async function revealDesktop(output: string, unlock: () => void) {
  const progress = signal(0);
  const handle = await COMPOSITOR.effect.overlay(output, {
    placement: "unlock",
    maxDuration: 3_000,
    effect: compileOverlayEffect({
      input: snapshotSource(),
      alpha: "preserve",
      pipeline: [shaderStage(dissolve, { uniforms: { progress } })],
    }),
  });
  try {
    unlock(); // ロッカーにロックの解除を伝える
    const start = Date.now();
    while (progress.value < 1) {
      progress.value = Math.min(1, (Date.now() - start) / 600);
      await new Promise<void>((resolve) => setTimeout(resolve, 8));
    }
  } finally {
    handle.dispose();
  }
}
```

アニメーションは組み込まれていません。シェーダーと、動きを減らす設定向けの代替は
設定側で用意してください。

## Rust

Rust SDK にも同じ API があります。

```rust
COMPOSITOR.session.configure_lock_background(LockBackground {
    window_ids: vec![window.id()],
    layer_ids: vec![],
})?;
let overlay = Overlay::new(effect)
    .placement(OverlayPlacement::Unlock)
    .max_duration_ms(3_000.0);
```

## 現状

新しい API です。選択、ロッカーが落ちたとき、リロード、受け渡しの動作は、ネスト（Winit）の
セッションで確認済みです。実際の TTY セッションでの本物のロッカー、VT 切り替え、DPMS、
複数・回転出力はまだ試していません。
