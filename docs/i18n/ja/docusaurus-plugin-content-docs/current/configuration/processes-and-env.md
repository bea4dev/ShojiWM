---
sidebar_position: 6
---

# プロセスと環境変数

## プロセスの起動

`COMPOSITOR.process` は外部プログラムを起動します。コンポジターがプロセスをどう
追跡するかによって、3つのメソッドがあります。

| メソッド | ライフサイクル | 用途 |
| --- | --- | --- |
| `once(id, spec)` | 起動時に1回実行 | セッションと共に始まるデーモン／エージェント |
| `service(id, spec)` | 常駐・監視・再起動 | 生かし続けたいウォッチャー |
| `spawn(spec)` | 起動して放置・追跡なし | オンデマンド起動（キーバインド） |

### `once`

セッション起動時にコマンドを1回実行します。

```ts
COMPOSITOR.process.once('fcitx5', {
  command: 'fcitx5 -d',
  runPolicy: 'once-per-session',
});
```

`runPolicy` は再実行を制御します。

- `"once-per-session"`（デフォルト） — ログインセッションごとに1回だけ実行。
- `"once-per-config-version"` — 設定変更後（ホットリロード）にも再実行。

### `service`

コンポジターが監視・再起動する常駐プロセスを起動します。

```ts
COMPOSITOR.process.service('cliphist-text', {
  command: ['wl-paste', '--type', 'text', '--watch', 'cliphist', 'store'],
  restart: 'on-exit',
});
```

`restart` ポリシー: `"never"`、`"on-failure"`（非ゼロ終了時のみ再起動）、
`"on-exit"`（常に再起動）。

### `spawn`

プロセスを起動して放置します――追跡も再起動もしません。キーバインドのハンドラ内に
最適です。

```ts
COMPOSITOR.key.bind('terminal', 'Super+T', () => {
  COMPOSITOR.process.spawn({command: ['kitty']});
});
```

### コマンド spec

どのメソッドも `command` を持つ spec を取り、`cwd` と `env` を任意で指定できます。

| フィールド | 型 | 意味 |
| --- | --- | --- |
| `command` | `string \| string[]` | 実行するプログラム（下記参照） |
| `cwd` | `string` | 作業ディレクトリ |
| `env` | `Record<string, string \| number \| boolean>` | このプロセス向けの追加環境変数 |

**`command` の解釈のされ方:**

- **単一文字列**は `/bin/sh -lc <command>` 経由で実行されるため、シェル機能（パイプ・
  リダイレクト・`~`・環境変数展開）が使えます。
  ```ts
  COMPOSITOR.process.spawn({command: 'hyprshot -m region --raw | swappy -f -'});
  ```
- **文字列配列**はシェルを介さず直接 exec されます――各要素が1つの argv エントリとして
  そのまま渡されます（安全で、クォートの落とし穴がありません）。
  ```ts
  COMPOSITOR.process.spawn({command: ['kitty', '--title', 'My Terminal']});
  ```

## ロックした状態で起動する

`COMPOSITOR.session.startLocked = true` を指定すると、セッションがロックされた状態で
コンポジターが起動します。ロッカーがロックして解除するまで、ext-session-lock の
サーフェス以外は描画されず、入力もそこにしか届きません。自動ログインと組み合わせると、
デスクトップを一瞬も見せずにロック画面をログイン画面として使えます。

ShojiWM 自身はロック画面を描かないので、設定からロッカーを起動してください。
ext-session-lock に対応したクライアントなら何でも使えます（Quickshell の
`WlSessionLock`、hyprlock、swaylock など）。

```ts
COMPOSITOR.session.startLocked = true;
COMPOSITOR.process.once('locker', { command: ['hyprlock'] });
```

- 設定のトップレベルで指定してください。設定の初回読み込み時に一度だけ読まれ、
  ホットリロードで動作中のセッションがロックされることはありません。
- ロッカーが接続するまで画面は黒いままです。
- 設定の読み込みに失敗した場合はロッカーも起動しないため、ロックせずに起動します。
- ロッカーが現れない場合でも、ロック中に `Ctrl+Alt+F<n>` で別の VT に切り替えられます。
- ロックを保持できるクライアントは 1 つだけです。他のクライアントのロック要求は拒否され、
  解除もできません。ロッカーが落ちた場合は、新しいロッカーが引き継ぐまでロックが続きます。
- Rust 設定では `ConfigBuilder::start_locked(true)` で同じことができます。

## 環境変数

`COMPOSITOR.env` は、コンポジターが起動するプロセスが継承する環境を管理します。変更は
呼び出し**後**に起動されるプロセスに反映されます。実行中のプロセスには `publish` しない
限り影響しません。

```ts
COMPOSITOR.env.set('QT_QPA_PLATFORM', 'wayland;xcb');

COMPOSITOR.env.apply({
  QT_IM_MODULE: 'fcitx',
  XMODIFIERS: '@im=fcitx',
  MOZ_ENABLE_WAYLAND: 1,
});
```

| メソッド | 意味 |
| --- | --- |
| `set(key, value)` | 変数を1つ設定（値は string/number/boolean） |
| `unset(key)` | 変数を削除 |
| `get(key)` | 現在値を取得（`string \| undefined`） |
| `apply(values)` | 一括設定。値に `null`／`undefined` を渡すとその変数を削除 |
| `publish(keys?)` | 現在の環境を実行中サービスにブロードキャスト。省略時は全キー |

:::tip
環境変数は、それを必要とするプロセスを起動する**前**に設定してください。デフォルト
設定では、ファイル冒頭付近で Qt プラットフォームや入力メソッドの変数を設定し、その後に
関連アプリを起動しています。
:::
