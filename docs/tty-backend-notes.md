# TTY Backend Notes

TTY backend で再発しやすい落とし穴のメモ。

## Cursor Buffer Reuse

### Symptom

- カーソル移動時にときどき固まる
- TTY backend だけ異常に重くなる
- 終了後もしばらく他のデスクトップ環境まで重く感じる
- `latest.log` が急速に肥大化する

### Root Cause

named cursor 用の `MemoryRenderBuffer` を毎フレーム新しく作ると、cursor element の underlying buffer/id が毎回変わる。

その結果:

- damage tracking が毎フレーム「変更あり」と判定する
- `queue_frame()` が止まらない
- idle 時でも redraw loop が回り続ける
- trace/debug が有効なときにログ I/O が爆発する

### Rule

TTY backend では named cursor の buffer を毎フレーム再生成してはいけない。

以下を守ること:

- xcursor frame ごとに `MemoryRenderBuffer` を cache する
- 同じ frame なら同じ buffer を再利用する
- cursor animation frame が変わったときだけ新しい buffer を作る

### Current Fix

`ShojiWM` は `pointer_images: Vec<(Image, MemoryRenderBuffer)>` を持ち、TTY 描画時に:

1. `cursor_theme.get_image(...)` で frame を得る
2. cache 内に同じ `Image` があればその `MemoryRenderBuffer` を再利用する
3. なければ `MemoryRenderBuffer::from_slice(...)` で新規生成して cache に追加する

### Why This Matters

Winit backend では同じ問題が見えなくても、TTY backend は DRM page flip と damage tracking が直接つながっているため、buffer identity の不安定さがすぐ redraw storm になる。

## Logging

TTY backend は per-frame のログを `debug` 以上に上げないこと。

特に以下は通常運用では `trace` に留める:

- redraw loop 進行
- frame queue
- render element count
- vblank notification

`RUST_LOG=shoji_wm=trace` で調査するときは、短時間で止めること。
