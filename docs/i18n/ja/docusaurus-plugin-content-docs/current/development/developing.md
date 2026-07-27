---
sidebar_position: 1
---

# 開発

このページでは、ローカルのチェックアウトから ShojiWM を実行し、性能を測定する方法を
解説します。コードベースの構成については
[アーキテクチャ概要](../architecture/shojiwm.md) を参照してください。

## ソースから実行する

ローカルのチェックアウトからは **`--dev --tty`** で ShojiWM を実行します。2つのフラグは
独立していて、組み合わせて使います。

- **`--dev`** は TypeScript 製の装飾ランタイムと設定を**ローカルリポジトリ**から読み込みます
  （`/usr` 配下のインストール済みコピーではなく）。これによりツリー内の変更が反映されます。
  リポジトリのルートで実行する必要があります。
- **`--tty`** は実機の TTY 上で **DRM/KMS バックエンド**を使います。デフォルトの winit
  バックエンドは現状不安定なので、`--tty` を推奨します。

空いている仮想端末（例: `Ctrl`+`Alt`+`F3`）に切り替えてログインし、リポジトリに `cd` してから
実行します。

```bash
cargo run -p shoji_wm -- --dev --tty
```

素の（デバッグ）ビルドはコンパイルが最速なので、機能を反復開発しているあいだはこれが
適しています。性能を測るときは `--release` でビルドしてください（後述）。

## ビルドプロファイルと性能

:::warning[性能は必ず --release で測ること]
デバッグビルドはリリースビルドより**桁違いに**遅く、しばしば 10 倍ほどの差があります。
デバッグビルドで得た性能値には意味がありません。性能を評価するときは必ず `--release` を
付けてください。

```bash
cargo run --release -p shoji_wm -- --dev --tty
```

:::

## プロファイリング

ShojiWM は、Rust 製コンポジターと RustyScript/Deno 製の装飾ランタイムを1つの
`shoji_wm` プロセス内で実行します。ヘルパースクリプト
[`tools/perf-top-functions.sh`](https://github.com/bea4dev/ShojiWM/blob/main/tools/perf-top-functions.sh)
は、Linux の `perf` でこのプロセスをプロファイリングします。

1. **リリース**ビルドで ShojiWM を起動し、測定したい負荷をかけます。
2. 実行中に、N 秒間（デフォルト `15`）プロファイリングします。

   ```bash
   tools/perf-top-functions.sh 20
   ```

   スクリプトは `shoji_wm` の PID を自動検出し、`perf` で記録して、self タイムと
   inclusive のトップ 10 シンボルのレポートを書き出します（`PIDS=<pid,pid>
tools/perf-top-functions.sh` のように対象を明示することもできます）。

:::note
`perf` はカーネル設定の緩和が必要なことがあります（例:
`sudo sysctl kernel.perf_event_paranoid=1`）。記録に失敗した場合は、スクリプトが
具体的な対処法を表示します。
:::

V8 の JIT フレームは `perf` 上で生のアドレスとして表示される場合があります。
コンポジターとランタイムプロトコルの境界を計測する場合は、ShojiWM の timescope
プロファイラを使用してください。
