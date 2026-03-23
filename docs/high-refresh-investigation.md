# High Refresh Rate Investigation

高リフレッシュレート環境での以下の症状についての調査メモ。

- カーソル移動のフレームスキップ / スタッタリング
- UFO test が 144 fps に張り付かず、60-110 fps 付近を揺れる
- UFO test を表示すると GPU 使用率が急増する

今回の前提として、同様の傾向は ShojiWM だけでなく Hyprland や Anvil / Smallvil 系でも観測され、niri では観測されない。

## Observed Symptoms

- UFO test 表示時に fps が高 Hz 出力の refresh rate に安定しない
- UFO test 表示時にカーソルのスタッタリングが顕著になる
- UFO test 表示時に GPU 使用率が大きく跳ねる
- 上記 3 つは同じタイミングで悪化する

このため、個別の不具合というより:

- フレーム供給タイミング
- redraw / damage の扱い
- frame callback throttling

のどれか、または複数が共通原因になっている可能性が高い。

## Prior Comparison Result

事前の比較では以下が確認されている。

- Hyprland / Anvil(Smithay) でも症状が出る
- niri では症状が出ない
- direct scanout や cursor plane を無効化しても、niri は安定する

この時点で、差は plane / scanout の有無そのものよりも:

- フレームをいつ出すか
- no-damage 時にどう振る舞うか
- frame callback をいつ誰に送るか

にある可能性が高い。

## Niri との差分で重要な点

### 1. redraw state machine がある

niri には output ごとの redraw state machine がある。

参照:
- `misc/niri/docs/wiki/Development:-Redraw-Loop.md`
- `misc/niri/src/niri.rs`

特に重要なのは:

- `Idle`
- `Queued`
- `WaitingForVBlank`
- `WaitingForEstimatedVBlank`

を分けて管理していること。

これにより:

- damage が出た frame は VBlank 完了待ち
- damage が出なかった frame でも即 Idle に戻らず
- 「次の VBlank 相当の時刻」まで待つ

という制御をしている。

これは単なる redraw の最適化ではなく、**frame callback throttling のために必須**だと niri 側 docs に明記されている。

### 2. no-damage 時も refresh cycle を進める

niri docs には次がはっきり書かれている。

- no-damage の redraw でも、推定 VBlank まで待つ
- そうしないと client が empty-damage commit busy loop を始める

ShojiWM の現状では、この「no-damage でも refresh cycle を管理する」層がかなり薄い。

### 3. frame callback を refresh cycle 単位で間引く

niri は `frame_callback_sequence` を持ち、同じ output refresh cycle 中に同じ surface へ callback を二重送信しない。

参照:
- `misc/niri/src/niri.rs`
- `send_frame_callbacks()`

要点:

- primary scanout output が一致している surface のみ対象
- 同じ cycle に同じ surface へ 2 回 callback を送らない

これにより:

- partially off-screen
- invisible surface
- empty damage commit

由来の busy loop を避けている。

## ShojiWM TTY Backend の現状

### 1. `queue_frame()` 後に手動で frame callback を送っている

ShojiWM TTY backend では、`render_frame()` に damage があった場合:

- `queue_frame(Some(output_presentation_feedback))`
- `surface.frame_pending = true`
- その直後に全 window / 全 layer に `send_frame()` を送る

参照:
- `src/backend/tty.rs`

重要なのは callback 判定が:

```rust
|_, _| Some(output.clone())
```

になっている点。

これは:

- 可視判定を使っていない
- primary scanout output 判定を使っていない
- refresh cycle 単位の重複防止がない

ということを意味する。

### 2. `post_repaint()` を使っていない

ShojiWM には `src/presentation.rs` に:

- `post_repaint()`
- `signal_post_repaint_barriers()`

がある。

winit backend は `post_repaint()` を使っているが、TTY backend は現状この共通経路を使わず、`tty.rs` 内で独自に callback を送っている。

この差はかなり大きい。

### 3. no-damage 時の estimated-vblank 待ちがない

ShojiWM TTY backend では、`render_frame()` が empty damage の場合は:

- `"tty frame had no damage"` として終わる

だけで、niri のような:

- `WaitingForEstimatedVBlank`
- estimated-vblank timer による refresh cycle 継続

は入っていない。

この差は UFO test のような client に直接効きやすい。

## Why UFO Test And Cursor Stutter Happen Together

この 2 つは同じ根から説明できる。

### 仮説

1. UFO test が高頻度で commit する
2. compositor が frame callback を粗く、または多めに返す
3. client がさらに redraw しやすくなる
4. redraw / callback / present scheduling が不安定になる
5. GPU 使用率が跳ねる
6. 同じ render loop にいる cursor も refresh slot を落としやすくなる

つまり:

- UFO test fps の不安定
- GPU 使用率の急増
- cursor のスタッタリング

は同じ現象の別の見え方である可能性が高い。

## Important Secondary Difference

ShojiWM TTY backend は frame flags もかなり保守的で:

- `FrameFlags::DEFAULT`

しか使っていない。

niri は状況に応じて:

- primary plane scanout
- cursor plane scanout
- overlay plane
- VRR 時の cursor-only update skip

などを調整している。

ただし今回の比較では:

- direct scanout を切っても niri は安定

なので、**第一原因は plane 最適化ではなく callback / redraw state machine 側**と考えるのが自然。

## Current Best Hypothesis

現時点で一番強い仮説は次の順。

1. **TTY backend の frame callback throttling が不足している**
2. **no-damage 時の estimated-vblank 待ちがない**
3. **TTY backend が `post_repaint()` ではなく独自 callback 送信をしている**
4. damage / redraw scheduling が refresh cycle 単位でまとまっていない

## Recommended Fix Order

実装順としてはこの順が安全。

### Step 1. TTY backend の callback 送信を `post_repaint()` 経路へ寄せる

まず `tty.rs` の manual `send_frame()` をやめ、可能な限り `src/presentation.rs` の共通経路へ寄せる。

狙い:

- winit / tty で callback 方針を揃える
- visible surface 判定と primary scanout output 判定を共通化する

### Step 2. no-damage 時の estimated-vblank throttling を入れる

niri の `WaitingForEstimatedVBlank` に相当する仕組みを ShojiWM TTY backend に入れる。

必要な性質:

- no-damage でも「今 refresh cycle が終わった」扱いにする
- すぐ次の redraw / frame callback を出さない
- おおよそ次の VBlank 時刻まで待つ

### Step 3. refresh cycle ごとの frame callback sequence を導入する

niri の `frame_callback_sequence` 相当を導入し、

- 同じ output refresh cycle 中
- 同じ surface

への二重 callback を抑止する。

### Step 4. 必要なら plane / cursor-only update 最適化を比較する

ここは second-order optimization として扱う。

今回の調査結果から、最初にやるべき場所ではない。

## Files To Revisit

ShojiWM 側:

- `src/backend/tty.rs`
- `src/presentation.rs`
- `src/handlers/compositor.rs`
- `src/handlers/layer_shell.rs`
- `src/state.rs`

niri 側の参考:

- `misc/niri/docs/wiki/Development:-Redraw-Loop.md`
- `misc/niri/src/niri.rs`
- `misc/niri/src/backend/tty.rs`

## Summary

今回の調査で一番重要なのは:

- 問題は direct scanout 単体ではなさそう
- niri との差は redraw state machine と frame callback throttling に強く見える
- ShojiWM TTY backend は現状、そこがかなり単純
- UFO test の不安定と cursor スタッタリングは同じ根で説明しやすい

まずは TTY backend の callback / no-damage / redraw-cycle 管理を niri に寄せていくのが本命。
