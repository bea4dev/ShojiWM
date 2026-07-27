---
sidebar_position: 2
---

# ライフサイクルとイベント

## ライフサイクルフック

設定モジュールはコンポジター起動時に読み込まれ、編集すると再読み込みされます
（ホットリロード）。2つのフックで初期化／後始末のコードを実行でき、**リロードを
またいで状態を引き継ぐ**ことができます。

```ts
COMPOSITOR.onEnable((event) => {
  if (event.isReloading) {
    const saved = event.restore<MyState>('my-state');
    if (saved) applyState(saved);
  }
});

COMPOSITOR.onDisable((event) => {
  if (event.isReloading) {
    event.persist('my-state', snapshotState());
  }
});
```

- **`onEnable(listener)`** — 設定が適用された後に実行されます。`event.isReloading` は
  ホットリロード時に `true`（新規セッションとの区別）。`event.restore<T>(key)` で前の
  バージョンが保存した状態を読めます。
- **`onDisable(listener)`** — 設定が破棄される前に実行されます。`event.persist(key,
  value)` で次のバージョンが復元できる状態を保存します。

この persist／restore のペアは、デフォルト設定がウィンドウマネージャのレイアウト
（ワークスペース、タイル状態）を編集をまたいでちらつきなく保持するために使っている
仕組みです。どちらも `COMPOSITOR.event.onEnable` / `onDisable` のショートハンドです。

## デバッグトグル

`COMPOSITOR.debug` は本番動作に影響しない開発専用のスイッチを持ちます。

```ts
COMPOSITOR.key.bind('fps', 'Super+Shift+F', () => {
  COMPOSITOR.debug.fpsCounter = !COMPOSITOR.debug.fpsCounter;
});
```

- **`fpsCounter: boolean`** — 各出力の左上に小さな FPS／フレーム時間オーバーレイを
  描画します。

## イベントバス

`COMPOSITOR.event` は、ウィンドウ・入力・出力・レイヤーの動きをカバーする `on*`
購読のバスです。すべての `on*` メソッドは**解除関数**を返します。リスナーは、ウィンドウを
レイアウトロジックに結びつける場所です。

```ts
COMPOSITOR.event.onOpen((window) => {
  console.log('opened', window.id);
});

COMPOSITOR.event.onFocus((window, focused) => {
  window.animation.start(focusVar, {to: focused ? 1 : 0, duration: ms(120)});
});
```

### ウィンドウのライフサイクル

| イベント | 発火タイミング |
| --- | --- |
| `onOpen(window)` | トップレベルウィンドウが作成されたとき |
| `onFirstCommit(window)` | 最初のバッファをコミットしたとき（表示可能になった） |
| `onFocus(window, focused)` | キーボードフォーカスを得た／失ったとき |
| `onStartClose(window)` | 閉じるシーケンスが始まったとき（閉じるアニメーションに最適） |
| `onClose(window)` | ウィンドウが破棄されたとき |

### ウィンドウからの要求

クライアントがコンポジターにウィンドウ状態の変更を求めたときに発火します。どう応じるかは
あなたのウィンドウマネージャが決めます。

| イベント | 発火タイミング |
| --- | --- |
| `onWindowResize(event)` | インタラクティブなリサイズが起きたとき |
| `onWindowMove(event)` | インタラクティブな移動が起きたとき |
| `onWindowMaximizeRequest(event)` | クライアントが最大化／解除を要求 |
| `onWindowMinimizeRequest(event)` | クライアントが最小化を要求 |
| `onWindowFullscreenRequest(event)` | クライアントがフルスクリーン／解除を要求 |
| `onWindowActivateRequest(event)` | クライアントがアクティブ化／フォーカスを要求 |

### 入力・出力・レイヤー

| イベント | 発火タイミング |
| --- | --- |
| `onPointerMove(event)` / `onPointerMoveAsync(event)` | ポインターが移動したとき |
| `onGestureSwipe(event)` / `onGestureSwipeAsync(event)` | マルチフィンガースワイプが進行したとき |
| `onOutputChange(event)` | 出力が追加／削除／再構成されたとき |
| `onInputDeviceChange(...)` | 入力デバイスのセットが変わったとき（ホットプラグ） |
| `onCreateLayer(...)` / `onUpdateLayer(...)` / `onDestroyLayer(...)` | レイヤーシェルサーフェス（バー／ドック／壁紙）がマップ／更新／アンマップされたとき |

### 同期・非同期の入力リスナー

`onPointerMove` と `onGestureSwipe` は、ネイティブランタイムブリッジを通じて
コンポジターの入力経路上で同期実行されます。ウィンドウ管理状態を最小遅延で更新する
短いハンドラではこちらを推奨します。ブロッキング I/O や重い処理は行わないでください。

`onPointerMoveAsync` と `onGestureSwipeAsync` はランタイムワーカーで実行され、
`Promise` を返せます。ランタイムが処理中の場合、高頻度な入力は途中のサンプルが
まとめられます。重い処理を行い、中間サンプルの欠落を許容できる場合に使用してください。

```ts
COMPOSITOR.event.onPointerMove((event) => {
  hybridWM.onPointerMove(event);
});
```
