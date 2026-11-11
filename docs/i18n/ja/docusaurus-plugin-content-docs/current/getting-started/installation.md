---
sidebar_position: 1
---

# インストール

ShojiWM は1つのスクリプト `dist/install.sh` でソースからインストールできます。
ビルド・コンポジターと TypeScript ランタイムのインストール・デフォルトのユーザー設定の
配置を行い、さらに Wayland セッションを登録するので、ログインマネージャーに ShojiWM が
表示されるようになります。

:::info[パッケージ版は準備中です]
ディストリビューション向けパッケージ（AUR など）は、**正式リリースの直前**に
登録する予定です。それまでは、下記の手順でソースからインストールしてください。
:::

## 前提条件

- 動作する Wayland / DRM 環境を備えた Linux システム
- 最近の Rust ツールチェーン（`cargo`）
- 以下のネイティブライブラリ（および開発用ヘッダー）。ShojiWM がリンクします。
  - `libwayland`
  - `libxkbcommon`
  - `libudev`
  - `libinput`
  - `libgbm`
  - `libseat`
  - `libxcb` と `xcb-util-cursor` —— 内蔵の xwayland-satellite が使います
  - `xwayland` —— X11 アプリの実行に使う Xwayland サーバー本体（下記の注記参照）
- `sudo` —— インストーラーが `/usr` にファイルをコピーし、セッションを登録するため

:::warning[最低バージョン]
ShojiWM は一部の LTS ディストリビューションより新しいシステムパッケージを必要とします。

- `libwayland` **1.23** 以上 —— 実行時に必須です。古いと起動直後に `NoWaylandLib` で終了します
- `xwayland` **23.1** 以上 —— 内蔵の xwayland-satellite に必要です（古い場合は
  `SHOJI_XWAYLAND_SATELLITE=off` で代替できます）
- PipeWire **0.3.77** 以上 —— xdg-desktop-portal バックエンドのビルドにのみ必要です
  （`--no-portal` で省略できます）

Ubuntu では **25.04 以降**が必要で、Ubuntu 22.04 / 24.04 はサポート対象外です。
Debian 13（trixie）以降も要件を満たします。
:::

:::note[ネイティブライブラリのインストール]
パッケージ名はディストリビューションによって異なります。例えば次のようになります。

```bash
# Debian / Ubuntu
sudo apt install libwayland-dev libxkbcommon-dev libudev-dev libinput-dev \
  libgbm-dev libseat-dev libxcb1-dev libxcb-cursor-dev xwayland

# Arch Linux
sudo pacman -S wayland libxkbcommon systemd-libs libinput mesa seatd libxcb \
  xcb-util-cursor xorg-xwayland
```

:::

:::note[X11 アプリについて]
ShojiWM は X11 アプリを
[`xwayland-satellite`](https://github.com/Supreeeme/xwayland-satellite) で動かします。
xwayland-satellite は ShojiWM に内蔵されています。
[`bea4dev/xwayland-satellite`](https://github.com/bea4dev/xwayland-satellite/tree/shojiwm)
の `shojiwm` ブランチにある ShojiWM 専用フォークを、コンポジターの中で動かすため、
`xwayland` 以外に別途インストールするものはありません。Xwayland が停止した場合は、
ShojiWM が同じ `DISPLAY` で自動的に起動し直します。

デバッグ用に、別途インストールした `xwayland-satellite` を独立したプロセスとして
動かすこともできます。

| 環境変数 | 動作 |
| --- | --- |
| `SHOJI_XWAYLAND_SATELLITE=external` | `PATH` 上の `xwayland-satellite` を起動する |
| `SHOJI_XWAYLAND_SATELLITE_PATH=/path/to/xwayland-satellite` | 指定したバイナリを起動する（`--xwayland-satellite-path` でも可） |
| `SHOJI_XWAYLAND_SATELLITE=off` | xwayland-satellite を使わず、Smithay 内蔵の Xwayland サポートを使う |
:::

## インストール

```bash
git clone https://github.com/bea4dev/ShojiWM.git
cd ShojiWM
./dist/install.sh
```

システムディレクトリへのコピーが必要になると、スクリプトが `sudo` を要求します。
スクリプトは次のことを行います。

- コンポジターと xdg-desktop-portal バックエンドを `cargo` で**ビルド**します。
- RustyScript を通して Deno/V8 TypeScript エンジンをコンポジターへ埋め込みます。
  実行時に Node.js は必要ありません。
- コンポジターを `/usr/bin/shoji_wm` に、ランタイムを `/usr/lib/shojiwm` に
  インストールします。
- `~/.config/shojiwm` に**デフォルトのユーザー設定**を作成します（既存の設定はそのまま
  残されます）。
- **Wayland セッションエントリ**を登録するので、**ログインマネージャーに ShojiWM が
  表示されます** —— ログイン画面で選ぶだけです。
- ShojiWM の **xdg-desktop-portal** バックエンド（スクリーンキャストとスクリーン
  ショット）をインストールし、この 2 つのポータルをそこへ振り向けます。それ以外は
  xdg-desktop-portal-gtk が担当します。

### ポータル経由のスクリーンショット

ポータルにスクリーンショットを求めるアプリ（ブラウザや Flatpak アプリ）には、出力
1 つ分の PNG が一時ディレクトリに作られます。`interactive` なしではフォーカス中の
ウィンドウがある出力で、フォーカスがなければ位置が最初の出力です。`interactive`
ではスクリーンキャストのピッカーが開き、ユーザーが出力を選びます。すべての出力は
リクエストが届いた時点でキャプチャするので、ピッカーが写り込むことはありません。
色の取得（`PickColor`）は未実装です。

xdg-desktop-portal（1.22 で確認）は、Access ポータルもある場合にだけスクリーンショットを
提供します。上記の gtk の既定がそれを提供します。

### インストールオプション

| フラグ        | 効果                                                |
| ------------- | --------------------------------------------------- |
| `--no-build`  | `cargo` のビルドをスキップし、既存のバイナリを使う  |
| `--no-portal` | xdg-desktop-portal バックエンドをインストールしない |
| `--no-config` | ユーザー設定の作成・更新を行わない                  |

`./dist/install.sh --help` でこの一覧を表示できます。

## NixOS / flakes

ShojiWM は実験的な Nix flake も提供しています。構成はソースインストーラーと同じ考え方で、
次のように分離します。

- コンポジター、portal バックエンド、組み込み Deno/V8 エンジン、TypeScript
  ランタイムのソースは Nix store に配置
- 編集する TypeScript 設定は `~/.config/shojiwm` に配置
- 開発時は引き続き `--dev` でソースツリーを直接参照

インストール済みコンポジターの実行時依存に Node.js は含まれません。Nix は ShojiWM の
ビルド時に、バージョンを固定した `rusty_v8` archive を固定出力依存として取得し、
コンポジター本体へリンクします。

:::warning[実験的機能]
NixOS 対応は追加直後です。常用環境で使う前に、リポジトリの最新状態を確認してください。
:::

### NixOS module (install)

ShojiWM を flake input に追加します。

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    shojiwm.url = "github:bea4dev/ShojiWM";
  };

  outputs = { nixpkgs, shojiwm, ... }: {
    nixosConfigurations.your-host = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        shojiwm.nixosModules.default
        {
          programs.shojiwm = {
            enable = true;
            initConfig = {
              enable = true;
              users = [ "your-user" ];
            };
          };
        }
      ];
    };
  };
}
```

その後、通常通り system configuration を適用します。

```bash
sudo nixos-rebuild switch --flake .#your-host
```

module は次をインストールします。

- `shoji_wm` コンポジター
- `xdg-desktop-portal-shojiwm`
- ログインマネージャー用の Wayland セッション
- スクリーンキャプチャ用の ShojiWM portal 設定

`programs.shojiwm.initConfig.enable = true` を設定すると、module は system
activation 時に指定ユーザーの ShojiWM TypeScript 設定ディレクトリを初期化します。
デフォルト config 一式は `src/index.tsx` がまだ存在しない場合にだけコピーされます。
既存の `src/index.tsx` や `src/window-manager.ts` などのユーザー設定ファイルは保持されます。
一方で、次の生成済みサポートファイルは rebuild のたびに同期されます。

- 現在の Nix store 内の TypeScript package を指す `node_modules/shoji_wm`
- `package.json`
- `tsconfig.json`

これらは、エディターの診断や単独での TypeScript 型チェックをインストール済みの
ShojiWM と同期させるために使われます。Node.js の実行時依存ではありません。
コンポジター本体は、埋め込み RustyScript/Deno ランタイムで config の解決と変換を
行います。

NixOS module に config directory を管理させたくない場合は `initConfig` を省略し、
編集可能な TypeScript 設定を手動で初期化します。

```bash
nix run github:bea4dev/ShojiWM#init-config
```

これにより、存在しない場合は `~/.config/shojiwm` が作成され、
`~/.config/shojiwm/node_modules/shoji_wm` が Nix store 内の package へリンクされます。
設定ファイル自体は書き換え可能なままなので、`Super` + `Shift` + `R` による
ホットリロードも使えます（[概要 → ホットリロード](../configuration/overview.md#ホットリロード) 参照）。

### 開発シェル

ShojiWM のソースツリーで次を実行します。

```bash
nix develop
cargo run --release -p shoji_wm -- --dev
```

リポジトリの TypeScript 型チェックやドキュメント開発ツールを使う場合に限り、
別途 `npm ci` を実行してください。

`--dev` では現在と同じくリポジトリ内のファイルを直接使います。

```text
./tools/decoration-runtime.ts
./packages/config/src/index.tsx
./packages/shoji_wm
```

つまり、Nix はネイティブ依存と固定済みの `rusty_v8` archive を揃えるために使い、
TS 設定や runtime の編集は今まで通り素早く試せます。

### 外部の xwayland-satellite

xwayland-satellite は ShojiWM に内蔵されています。デバッグや別ブランチの試用などで、
別途パッケージ化した `xwayland-satellite` を独立したプロセスとして動かしたい場合は、
NixOS module で有効にします。

```nix
{
  programs.shojiwm = {
    enable = true;
    xwaylandSatellite.enable = true;
    xwaylandSatellite.package =
      inputs.xwayland-satellite-shojiwm.packages.${pkgs.system}.default;
  };
}
```

flake input には、例えば次のように package を追加します。

```nix
{
  inputs.xwayland-satellite-shojiwm.url =
    "github:bea4dev/xwayland-satellite/shojiwm";
}
```

`programs.shojiwm.xwaylandSatellite.package` には、`bin/xwayland-satellite` を提供する任意の
package を指定できます。既定値は `pkgs.xwayland-satellite` です。

## 実行

- **ログインマネージャーから:** セッションとして **ShojiWM** を選んでログインします。
- **TTY から:** `shoji_wm --tty` を実行します。
- **開発（ネストしたウィンドウ）:** ソースツリーで
  `cargo run --release -p shoji_wm -- --dev` を実行します。現在のセッションを抜けずに
  反復開発できて便利です。

## オプション: デスクトップシェル

ShojiWM はコンポジター単体であり、バーやランチャーなどのシェル UI を自前では同梱して
いません。標準のシェル実装は別途提供されています。

- **shoji-bar-4** —— [github.com/bea4dev/shoji-bar-4](https://github.com/bea4dev/shoji-bar-4)。デフォルト設定のウィンドウの枠と揃った、Material 3 Expressive のシェルです

インストールと有効化の手順は、そのリポジトリの `README.md` を参照してください。
（ShojiWM のデフォルト設定は、`~/.config/shoji-bar-4/target/release/shoji-bar-4` が存在すれば自動的に起動します。）
