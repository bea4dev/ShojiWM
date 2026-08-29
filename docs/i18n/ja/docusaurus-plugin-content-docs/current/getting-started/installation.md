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
  - `xwayland` —— Xwayland サーバー本体（下記の `xwayland-satellite` が利用します）
- [`xwayland-satellite`](https://github.com/Supreeeme/xwayland-satellite) ——
  X11 / Xwayland アプリの実行に必要（下記の注記参照）
- `sudo` —— インストーラーが `/usr` にファイルをコピーし、セッションを登録するため

:::note[ネイティブライブラリのインストール]
パッケージ名はディストリビューションによって異なります。例えば次のようになります。

```bash
# Debian / Ubuntu
sudo apt install libwayland-dev libxkbcommon-dev libudev-dev libinput-dev \
  libgbm-dev libseat-dev xwayland

# Arch Linux
sudo pacman -S wayland libxkbcommon systemd-libs libinput mesa seatd xorg-xwayland
```

:::

:::note[xwayland-satellite は必須です]
ShojiWM は X11 アプリの実行に `xwayland-satellite` を使用します。推奨は、リポジトリを
クローンして Cargo で直接インストールする方法です。

```bash
git clone https://github.com/Supreeeme/xwayland-satellite.git
cd xwayland-satellite
cargo install --path ./
```

これで `xwayland-satellite` バイナリが `PATH`（通常は `~/.cargo/bin`）に置かれます。
セッションを起動する前にインストールしておいてください。

**ShojiWM 向けの推奨:** ホットフィックスを含む ShojiWM 専用のフォークが、
[`bea4dev/xwayland-satellite`](https://github.com/bea4dev/xwayland-satellite/tree/shojiwm)
の `shojiwm` ブランチにあります。Unity のタブを掴んで移動できない問題への試験的な修正が
含まれています。これらの修正やその他のホットフィックスのサポートが必要な場合は、こちらの
ブランチをインストールすることを推奨します。

```bash
git clone -b shojiwm https://github.com/bea4dev/xwayland-satellite.git
cd xwayland-satellite
cargo install --path ./
```

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
- ShojiWM の **xdg-desktop-portal** バックエンド（スクリーンキャストなど）を
  インストールします。

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
- nixpkgs に存在する場合は `xwayland-satellite`

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

### xwayland-satellite fork

ShojiWM 向けの `xwayland-satellite` fork が必要な場合は、NixOS module の package option
で差し替えます。

```nix
{
  programs.shojiwm = {
    enable = true;
    xwaylandSatellite.package =
      inputs.xwayland-satellite-shojiwm.packages.${pkgs.system}.default;
  };
}
```

flake input には、例えば次のように fork を追加します。

```nix
{
  inputs.xwayland-satellite-shojiwm.url =
    "github:bea4dev/xwayland-satellite/shojiwm";
}
```

`programs.shojiwm.xwaylandSatellite.package` には、`bin/xwayland-satellite` を提供する任意の
package を指定できます。

## 実行

- **ログインマネージャーから:** セッションとして **ShojiWM** を選んでログインします。
- **TTY から:** `shoji_wm --tty` を実行します。
- **開発（ネストしたウィンドウ）:** ソースツリーで
  `cargo run --release -p shoji_wm -- --dev` を実行します。現在のセッションを抜けずに
  反復開発できて便利です。

## オプション: デスクトップシェル

ShojiWM はコンポジター単体であり、バーやランチャーなどのシェル UI を自前では同梱して
いません。標準のシェル実装は別途提供されています。

- **shoji-bar-2** —— [github.com/bea4dev/shoji-bar-2](https://github.com/bea4dev/shoji-bar-2)

インストールと有効化の手順は、そのリポジトリの `README.md` を参照してください。
（ShojiWM のデフォルト設定は、`shoji-bar-2` が存在すれば自動的に起動します。）
