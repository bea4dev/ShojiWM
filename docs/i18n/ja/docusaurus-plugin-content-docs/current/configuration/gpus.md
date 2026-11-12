---
sidebar_position: 4.8
---

# GPU

`COMPOSITOR.gpu` は、シートの GPU の一覧を返し、すべての合成に使う GPU（**合成用 GPU**）
を選び、プログラムを特定の GPU で描画させるための環境変数を返します。GPU を報告するのは
TTY バックエンドだけで、ネスト実行中は一覧が空のままで、`selectRender` も呼ばれません。

## ShojiWM での複数 GPU の扱い

GPU はすべて開きます。まだ何も繋がっていない GPU も開くので、あとからどの GPU に
ディスプレイを挿しても使えます。合成は**1台**の GPU（合成用 GPU）で行い、クライアントの
バッファもその GPU 向けに確保されます。他の GPU に繋がったディスプレイにもデスクトップは
表示されます。その画面のフレームは合成用 GPU で LINEAR 形式（どのベンダーでも読める
メモリ配置）のバッファに合成され、表示側の GPU はそのバッファを画面に出すだけです。

設定で選ばなければ、ShojiWM が次の順で合成用 GPU を選びます。

1. 内蔵パネル（`eDP`、`LVDS`、`DSI`）を表示している GPU（ハイブリッドノートでは内蔵 GPU）
2. ファームウェアが起動画面を表示した GPU（`bootVga`）
3. 繋がっているディスプレイが最も多い GPU

## GPU の情報を読む

```ts
import {COMPOSITOR} from 'shoji_wm';

COMPOSITOR.gpu.onChange((gpus) => {
  for (const gpu of gpus) {
    console.log(gpu.id, gpu.name, gpu.driver, gpu.outputs, gpu.isRender);
  }
});
```

| メンバー | 戻り値 |
| --- | --- |
| `list` | `GpuInfo[]` — すべての GPU（`id` 順） |
| `render` | `GpuInfo \| undefined` — 合成用 GPU |
| `get(id)` | `GpuInfo \| undefined` |
| `onChange(listener)` | GPU の追加・取り外し、ディスプレイの接続・切断のとき、および enable/reload 後の最初に、すべての GPU を渡して呼ばれます |
| `selectRender(resolver)` | 合成用 GPU を選ぶ（後述） |
| `offloadEnv(gpu)` | プログラムを `gpu` で動かすための環境変数（後述） |

`list` は `onEnable` 後の最初の報告までは空です。設定のトップレベルで読むのではなく、
`onChange` で受け取ってください。

`GpuInfo` の内容:

| フィールド | 意味 |
| --- | --- |
| `id` | GPU の接続位置（例: `"pci-0000:03:00.0"`。udev の `ID_PATH`）。再起動しても変わらないので、**GPU を指すときはこれを使います。** |
| `dynamicCardId` | `"card1"`。認識された順に番号が付くため、起動ごとや抜き差しで変わることがあります。ログや表示専用です。 |
| `renderNode` | `"/dev/dri/renderD129"`（番号は `dynamicCardId` と同じく変わりえます）、または `null` |
| `vendor` | `"intel"`、`"amd"`、`"nvidia"`、`"other"` のいずれか |
| `vendorId`, `deviceId` | PCI の ID。PCI 以外の GPU では 0 |
| `driver` | カーネルドライバ: `"i915"`、`"xe"`、`"amdgpu"`、`"nvidia"`、`"nouveau"` など |
| `name` | ハードウェアデータベースのモデル名（例: `"Navi 32 [Radeon RX 7800 XT]"`） |
| `bootVga` | ファームウェアが起動画面を表示した GPU（ハイブリッドノートでは通常は内蔵 GPU） |
| `removable` | カーネルが取り外し可能と報告している GPU（Thunderbolt の eGPU など） |
| `outputs` | ディスプレイが繋がっているコネクタ。出力と同じ名前（`"DP-1"`）です |
| `isRender` | すべての合成をこの GPU で行っています |
| `inUse` | ShojiWM がこの GPU を開いています。`SHOJI_TTY_DRM_DEVICE` で除外された GPU は `false` です |

「内蔵 GPU かどうか」を示すフィールドはありません。Linux には信頼できる判定方法がない
（AMD の APU は他の GPU と見分けがつかない）ためです。GPU の区別には `bootVga`、`driver`、
`vendor`、`outputs`、`id` を使ってください。

## 合成用 GPU を選ぶ

```ts
COMPOSITOR.gpu.selectRender((gpus) =>
  gpus.find((gpu) => gpu.vendor === 'amd' && !gpu.bootVga) ?? 'auto',
);
```

resolver は `GpuInfo`、`id`（`"card1"`、`"/dev/dri/card1"`、レンダーノード、`DRI_PRIME`
形式の `"pci-0000_03_00_0"` も可）を返すか、`"auto"` を返すか何も返さなければ ShojiWM の
自動選択になります。

- **起動時に一度だけ、`onEnable` より前・どの GPU も開く前に**呼ばれます。設定の
  トップレベルで登録してください。変更は次回の起動から反映され、ホットリロードでは
  GPU は切り替わりません。
- **GPU が 2 台以上あるときだけ**呼ばれます。GPU が 1 台のマシンでは、設定を読み込む
  タイミングも含めて何も変わりません。
- resolver の中では、`isRender` は ShojiWM が自動で選ぶ GPU を、`inUse` はこれから開く
  GPU を示します。
- 開く予定のなかった GPU を選ぶと、その GPU も開きます。ただし `SHOJI_TTY_DRM_DEVICE`
  で開く GPU を指定している場合はそちらが優先され、選択は警告付きで無視されます。
- どの GPU にも一致しない値や、resolver が投げた例外は、自動選択に戻したうえで設定エラー
  として表示されます。
- 選んだ GPU の初期化に失敗した場合（ドライバを読み込めないなど）は次の GPU が引き継ぐ
  ので、選択を誤ってもセッションが起動できなくなることはありません。

環境変数 `SHOJI_RENDER_GPU`（resolver の戻り値と同じ形式）は設定より優先されます。
設定を書き換えずに GPU を試したいときに使えます。`SHOJI_RENDER_GPU=auto` にすると、
設定の選択を無視して自動選択になります。

```sh
SHOJI_RENDER_GPU=pci-0000:03:00.0 shoji_wm --tty
```

### 代償

他の GPU に繋がったディスプレイには、毎フレームのフレームをコピーして渡します（PCIe を
またぐ LINEAR のコピー）。ハイブリッドノートで dGPU を合成用にすると内蔵パネルがこの
扱いになり、セッション中は dGPU が省電力状態に入れなくなります。

NVIDIA のプロプライエタリドライバは LINEAR の dmabuf に直接描画できません。合成用 GPU が
NVIDIA の場合は、LINEAR の GBM サーフェスに描画します。合成そのものは NVIDIA 本来の
メモリ配置で行い、スワップ時にドライバが LINEAR へ 1 回コピーします（NVIDIA 自身の PRIME
レンダーオフロードと同じ方式）。セッションログには
`rendering cross-gpu frames into a LINEAR gbm surface` と出ます。

こうしたディスプレイを表示する GPU は、合成用 GPU のフレームの完成を自分で（フレームの
フェンスを使って）待ちます。そのため CPU はすぐ次の作業に戻れ、
[`framePacing = "throughput"`](outputs.md#フレームの出し方トリプルバッファ) による
「GPU の描画と次のフレームの CPU 処理の重ね合わせ」も維持されます。これには両方の
ドライバが `EGL_ANDROID_native_fence_sync` に対応している必要があり、対応していなければ
CPU が合成用 GPU を待ちます。その場合も動作はしますが、待っている間はイベントループが
止まります。どちらの方式になったかはセッションログに出ます（`cross-gpu frames: ...`）。
ドライバの組み合わせによってフェンスの受け渡しに問題が出る場合は、
`SHOJI_CROSS_GPU_CPU_WAIT` を設定すると CPU で待つ方式に固定できます。

## プログラムを特定の GPU で動かす

`offloadEnv(gpu)` は PRIME レンダーオフロード用の環境変数を返します。
`COMPOSITOR.process.spawn` に渡して使います。

```ts
COMPOSITOR.key.bind('steam', 'Super+G', () => {
  const dgpu = COMPOSITOR.gpu.list.find((gpu) => !gpu.bootVga);
  COMPOSITOR.process.spawn({
    command: 'steam',
    env: dgpu ? COMPOSITOR.gpu.offloadEnv(dgpu) : {},
  });
});
```

| ドライバ | 環境変数 |
| --- | --- |
| `nvidia`（プロプライエタリ） | `__NV_PRIME_RENDER_OFFLOAD=1`、`__GLX_VENDOR_LIBRARY_NAME=nvidia`、`__VK_LAYER_NV_optimus=NVIDIA_only` |
| Mesa のドライバ（`i915`、`xe`、`amdgpu`、`nouveau` など） | `DRI_PRIME=<id を udev タグ形式にしたもの>`、`MESA_VK_DEVICE_SELECT=<vendor>:<device>` |

これで変わるのは、プログラムが描画に使う GPU だけです。ウィンドウの合成は引き続き合成用
GPU で行われます。

## Rust の設定

```rust
COMPOSITOR.gpu.select_render(|gpus| {
    gpus.iter()
        .find(|gpu| gpu.vendor == GpuVendor::Amd && !gpu.boot_vga)
        .map_or(RenderGpuChoice::Auto, |gpu| RenderGpuChoice::Gpu(gpu.id.clone()))
});

effect(|| {
    for gpu in COMPOSITOR.gpu.list().get() {
        println!("{} {:?}", gpu.id, gpu.name);
    }
});
```

`list()` はシグナルで、`render()` と `get(id)` はそれを読みます。`offload_env(&gpu)` は
同じ環境変数をキーと値の組で返します。
