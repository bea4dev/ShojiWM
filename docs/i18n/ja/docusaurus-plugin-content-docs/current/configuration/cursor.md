---
sidebar_position: 4.5
---

# カーソル

`COMPOSITOR.cursor` は、デフォルトポインターやSSDのリサイズカーソルなど、
ShojiWMが所有するnamed cursorのXCursorテーマを設定します。
[カーソルの移動](#カーソルの移動)や
[アプリのポインターロックの無視](#アプリのポインターロックを無視する)もここで行います。

```ts
COMPOSITOR.cursor.configure({
  theme: 'Bibata-Modern-Classic',
  size: 24,
});
```

`theme` にはインストール済みXCursorテーマ名を指定します。`size` は1から512の
整数で、単位は**論理ピクセル**です。ShojiWMは出力ごとに適切にスケールされた画像を
選ぶため、24を指定したカーソルはscale 1とscale 2の出力で同じ論理サイズになります。

`configure()` はShojiWMを再起動せずに反映され、同時に次の処理を行います。

- 後から起動するプロセス向けに `XCURSOR_THEME` と `XCURSOR_SIZE` を設定する
- これらの変数をsystemdおよびD-Busのactivation environmentへ公開する
- ShojiWMがデコードしたカーソル画像とGPUバッファのキャッシュを破棄する
- 次の再描画から新しいnamed cursorを使用する

## テーマファイルの再読み込み

テーマ名やサイズを変えずにテーマファイルだけを置き換えた場合は、現在のテーマを
明示的に再読み込みできます。

```ts
COMPOSITOR.cursor.reload();
```

キーバインドから呼び出すこともできます。

```ts
COMPOSITOR.key.bind('reload-cursor', 'Super+Shift+C', () => {
  COMPOSITOR.cursor.reload();
});
```

## クライアント所有のカーソルサーフェス

Waylandクライアントは独自のカーソルサーフェスを送信できます。この場合、ShojiWMは
クライアントから渡されたサーフェスを表示する必要があり、named cursorへ強制的に
置き換えることはできません。

公開したXCursor環境変数により、新しく起動したツールキットは同じテーマを選択できます。
一方、すでに起動しているアプリケーションのクライアント所有カーソルを変更するには、
アプリケーションの再起動や、そのアプリケーション側での設定再読み込みが必要な場合があります。

カーソル設定を変更しても、クライアント所有のカーソルサーフェスは置換・加工されません。

## カーソルの移動

`setPosition` はカーソルを移動します。全体の論理座標か、出力上の点を指定します。
出力上の点は [ポインター領域](./keybindings-and-pointer.md#ポインター領域) と同じ
方法で決まります。まず `anchor`（既定 `"center"`）で置き、`offset`（x は右、
y は下が正）でずらします。

```ts
COMPOSITOR.cursor.setPosition({x: 800, y: 500});
COMPOSITOR.cursor.setPosition({output: 'DP-1', anchor: 'center'});
COMPOSITOR.cursor.setPosition({output: 'DP-1', anchor: 'top-right', offset: {x: -40, y: 40}});

const position = COMPOSITOR.cursor.getPosition();
// { x, y, outputName } — ポインターが一度も動いていない間は undefined
```

- クライアントには通常の移動として届きます。ホバーやポインターフォーカスは
  カーソルに追従し、ポインター領域の `enter` / `leave` も通知されます。
  `onPointerMove` は**発火しません**。カーソルを動かすリスナーが自分自身を
  呼び続けることはありません。
- どの出力にも入らない点は、最も近い出力に収められます。出力を指定した場合は
  その出力の中に収まります。
- アプリがポインターをロック・閉じ込めしている間（ゲームなど）、ボタンを
  押している間（ドラッグ中）、画面ロック中は無視されます。
- `getPosition()` は直近のポインター移動時点の位置を、コンポジターへ問い合わせずに
  返します。どこから呼んでも軽い処理です。

```ts
// カーソルを次の出力の中央へ移動
COMPOSITOR.key.bind('cursor-next-output', 'Super+Ctrl+Right', () => {
  const outputs = COMPOSITOR.output.list;
  const current = COMPOSITOR.cursor.getPosition()?.outputName;
  const next = outputs[(outputs.indexOf(current ?? '') + 1) % outputs.length];
  if (next) COMPOSITOR.cursor.setPosition({output: next});
});
```

カーソルを動かせるのはあなたの設定だけです。Wayland のクライアントには
カーソルを動かす手段がなく、この API もクライアントには公開されません。

## アプリのポインターロックを無視する

ゲームなどのアプリは、カーソルをその場に固定したり、ウィンドウ内に閉じ込めたり
できます。その間はカーソルがウィンドウの外へ出られないため、ゲームを終了しないと
`Super` でゲームのウィンドウをドラッグすることも、別の出力のバーへ行くことも
できません。

`bindAppLockIgnoreModifier(modifier)` は、`modifier` を押している間そのロックを
無視します。デフォルト設定では `Super` を使っています。

```ts
COMPOSITOR.cursor.bindAppLockIgnoreModifier('Super');
```

`Super` を押すとカーソルが自由になり、押したままゲームのウィンドウをドラッグする
こともできます。ゲームの上で離すとロックが戻ります。

自分で切り替えるときは `setAppLockIgnored` を使います。

```ts
COMPOSITOR.cursor.setAppLockIgnored(true);   // アプリのロックを無視
COMPOSITOR.cursor.setAppLockIgnored(false);  // 再び従う
```

ロックを無視している間は次のようになります。

- **アプリにはロック解除が通知されます。** 多くのゲーム（SDL など）はロックを
  保持していて、自分で取り戻します。ブラウザのポインターロックのような1回限りの
  ロックは、カーソルがウィンドウを出たときと同じく消えるので、ページを
  クリックし直してください。
- **カーソルはアプリが指定した位置に現れます。** アプリはロック解除時のカーソル
  位置（照準など）を指定でき、ShojiWM はそれに従います。
- **アプリには生のマウス移動が届かなくなります。** カーソルをゲームの上で動かしても、
  ゲームの視点は回りません。
- **そのアプリの上では、アプリのカーソルの代わりに ShojiWM の既定の矢印を表示します。**
  ゲームはロック中にカーソルを隠していることが多いため、これでロックが外れていることが
  分かります。それ以外の場所では、カーソルはいつもどおりです。

モディファイア自体は、`Super` + ドラッグのときと同じく、フォーカス中のアプリに
キーとして届きます。
