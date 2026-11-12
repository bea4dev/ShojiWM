---
sidebar_position: 4.7
---

# 電源と蓋

`COMPOSITOR.power` は、systemd-logind にサスペンドやハイバネートなどを依頼し、
システムが復帰したことを設定に知らせます。また、ノートパソコンの蓋の処理を logind から
引き受けられるようにします。logind とやり取りするのは TTY バックエンドだけで、
ネスト実行中はこれらの呼び出しは何もしません。

## 蓋を処理するのは誰か

logind は蓋のスイッチを自分で読み、`logind.conf` の設定どおりに動作します（既定は
`HandleLidSwitch=suspend`）。ShojiWM も同じ瞬間に libinput 経由で蓋のことを知りますが、
両者の順序は決まっていません。そのため、[`onLidSwitch`](input.md#ノートパソコンの蓋)
を受け取るだけの設定では logind のサスペンドを止められず、logind より先に動ける保証も
ありません。

蓋の動作を設定で決めるには、蓋を logind から引き受けます。

```ts
COMPOSITOR.power.takeOver({lid: true});
```

ShojiWM が `handle-lid-switch` の inhibitor を持ち、logind は蓋に反応しなくなります。
以後、設定が何かしない限り、蓋を閉じても何も起きません。inhibitor はファイル記述子なので、
ShojiWM が終了すれば logind はすぐに蓋の処理を取り戻します。設定の他の部分と同じく、
リロードのたびにセットアップで宣言し直してください。宣言しなかったリロード後の設定や、
読み込みに失敗した設定では、蓋は logind に戻ります。

## デフォルト設定の蓋のポリシー

デフォルト設定は蓋を引き受け、Windows や KDE Plasma と同じく、閉じたときの動作を
バッテリー駆動時と外部電源接続時の 2 つで設定します。中身は普通の設定コード
（`packages/config/src/lid-policy.ts` の `bindLidPolicy`）なので、自由にコピーしたり
変更したりできます。

```ts
bindLidPolicy({
  onBattery: 'logind',
  onExternalPower: 'logind',
});
```

`"logind"` は、その状況で `logind.conf` が指定している動作をします
（`HandleLidSwitch`・`HandleLidSwitchExternalPower`、他のディスプレイが点いている間は
`HandleLidSwitchDocked`）。そのため、初期状態では蓋の動作は今までと変わりません。
自分で決めるにはアクションを指定します。

```ts
bindLidPolicy({
  onBattery: 'hibernate',
  onExternalPower: 'suspend',
  onSleep: async () => {
    // スリープする前にスクリーンロッカーを出す。
  },
  onResume: ({lidClosed}) => {
    // サスペンドやハイバネートから戻った。
  },
});
```

| オプション | 既定値 | 意味 |
| --- | --- | --- |
| `onBattery` | `"logind"` | バッテリー駆動時に蓋を閉じたときの動作。 |
| `onExternalPower` | `"logind"` | 外部電源接続時に蓋を閉じたときの動作。 |
| `onSleep` | なし | ポリシーがスリープさせる直前（`suspend`・`hibernate`・`hybrid-sleep`・`suspend-then-hibernate`・`sleep`）に `{action, externalPower, externalDisplay}` を渡して呼ぶ。Promise を返すと最大 `sleepCallbackTimeoutMs` まで待つ。例外が出てもスリープはする。 |
| `onResume` | なし | 復帰のたびに（誰がスリープさせたかによらず）`{lidClosed}` を渡して呼ぶ。 |
| `internalPanelOff` | `true` | 蓋が閉じていて他のディスプレイが点いている間は内蔵パネルを消灯（DPMS）し、蓋が開くかそのディスプレイが無くなったら点け直す。 |
| `holdoffMs` | `30000` | ポリシーの開始直後と復帰直後のこの時間は、蓋を閉じても何もしない（logind の `HoldoffTimeoutSec`）。 |
| `sleepCallbackTimeoutMs` | `5000` | `onSleep` を待つ最長時間（logind の `InhibitDelayMaxSec`）。 |

アクションには、[logind のアクション](#電源の操作)、`"ignore"`、`"logind"`、または
`{externalPower, externalDisplay}` を受け取って自分で判断する関数を指定できます。
「他のディスプレイ」とは、内蔵パネル（`eDP`・`LVDS`・`DSI`）でもヘッドレス出力でもない、
有効な出力のことです。蓋を閉じたまま最後の外部ディスプレイを外すと、logind と同じく、
新しい状況のアクションを実行します。`bindLidPolicy` は蓋を logind に返す関数を返します。

### 壊れた蓋スイッチ

ノートパソコンによっては、蓋のスイッチが「閉じている」のまま固まります。スイッチが
閉じていると言うたびにサスペンドするポリシーでは、こうした機械は起動直後と、復帰するたびに
すぐにまたスリープしてしまいます。このポリシーは、蓋が実際に動いたときだけアクションを
実行します。

- スイッチが立ち上がったとき（起動時や別の VT から戻ったとき）や、リロード後の設定が
  始まったときにすでに閉じている蓋では何もしません。この蓋のイベントは `initial: true` で
  届きます。内蔵パネルの消灯だけはこれに従うので、ドックで蓋を閉じたまま起動しても
  パネルは消えたままです。
- 復帰すると待ち時間（holdoff）が始まり直し、復帰後も閉じていると読める蓋は、
  再び動くまで何も実行しません。
- 内蔵キーボードのキーが押されると、libinput は蓋を「開いた」と報告します。固まった
  スイッチも、誰かが何か入力すれば元に戻ります。

libinput が知らない機種では、ローカルの quirk で、起動時のスイッチの状態を信用しないよう
指定するか（`unreliable`）、最初のキー入力でカーネルに「開いた」を書き戻すよう指定できます
（`write_open`）。`write_open` は logind 側の判断も正しくします。

```ini
# /etc/libinput/local-overrides.quirks
[My laptop's lid switch]
MatchName=*Lid Switch*
MatchDMIModalias=dmi:*svnVENDOR:pnMODEL:*
AttrLidSwitchReliability=write_open
```

## 電源の操作

```ts
COMPOSITOR.power.suspend();
COMPOSITOR.power.hibernate();
COMPOSITOR.power.perform('suspend-then-hibernate');
```

`perform` には logind のアクション名を指定します。`suspend`、`hibernate`、
`hybrid-sleep`、`suspend-then-hibernate`、`sleep`（logind に設定されたスリープ。
systemd 256 より前ではサスペンド）、`lock`（`loginctl lock-sessions` と同じく、
各セッションのスクリーンロッカーにロックを依頼）、`poweroff`、`reboot`、`halt` です。
どれも対話的ではないので、polkit がパスワードを求める場合は logind が拒否し、
そのことがログに残ります。

## 状態と復帰

```ts
COMPOSITOR.power.onStateChange(({externalPower, logindLidActions}) => {
  console.log(externalPower ? '充電中' : 'バッテリー駆動', logindLidActions);
});
COMPOSITOR.power.onResume(({lidClosed}) => {
  // サスペンドやハイバネートからの復帰（誰が始めたかによらない）。
});
```

`COMPOSITOR.power.state` は最新の状態を持ちます。`externalPower`（外部電源で動いているか。
systemd と同じく、オンラインのアダプターがあるか、アダプターが一つも無ければ外部電源とみなします）
と `logindLidActions`（logind の蓋に関する 3 つの設定）です。どちらも分かるまでは
`undefined` です。状態は蓋と同じくスケジューラーのティックで届き、同じティックの
蓋のイベントより先に届きます。

`onResume` は復帰のたびに呼ばれます。`lidClosed` は蓋のスイッチが最後に報告した状態で、
固まったスイッチは閉じていると報告し続けることがあります。復帰と蓋が開いた通知の
どちらが先に届くかは決まっていないので、どちらのハンドラーも順序に頼らず、
今の状態に揃える書き方にしてください。

## Rust の設定では

`COMPOSITOR.power` に同じ API があります。

```rust
COMPOSITOR.power.take_over(PowerTakeOverOptions { lid: true });
COMPOSITOR.power.on_resume(|resume| println!("resumed, lid {:?}", resume.lid_closed));
let power = COMPOSITOR.power.state(); // シグナル
```

`take_over`・`suspend`・`hibernate`・`perform` は TypeScript のメソッドと対応し、
`state()` は最新の状態を持つシグナルです（最初の報告までは `None`）。
イベントはスケジューラーのティックを待たずにすぐ届きます。

Rust の設定例にも同じ蓋のポリシーが
`src/shojiwm_rs/examples/default_config/lid_policy.rs` にあり、`on_battery`・
`on_external_power`・`on_sleep`・`on_resume` を指定できます。Rust の設定は同期的に
動くので、`on_sleep` は待たれるのではなく、呼び出しが戻った時点でスリープに進みます。
