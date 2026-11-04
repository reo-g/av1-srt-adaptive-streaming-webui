# 用語の言い換え対応表

画面にも API にも「SRT」の名前や、SRT 独自の言葉は出しません。設定項目は「何をするものか」で名付け、ほかの伝送方式でも使う一般的な用語はそのまま使います。実装の内部（設定ファイルの内部表現、開発者向けログ）では元の用語を使ってかまいません。

使う人は技術者ではありません。画面の言葉は「いま何が起きているか」「どうすればよいか」が分かるふつうの日本語にします。専門用語が避けられないときは、短い説明を添えます。

元の対応表は Claude の文書「用語の言い換え対応表」です。この表はそれに、API で使う名前を加えたものです。

## 区分

- **必ず置き換える**：「SRT」の文字、SRT 独自の接続モード名（Caller / Listener / Rendezvous）、SRT 独自の設定名（SRT 遅延）。
- **置き換えを推奨**：言葉は一般的でも、組み合わせると SRT だと分かる表現（パスフレーズの文字数「10〜79 文字」など）。また、ふつうの人に通じにくい言葉（ビットレート、パケット、RTT、帯域、エンコード、fps など）。
- **そのまま使う**：暗号化方式（AES-128 など）、伝送、受信機・送信機、映像の形式名（AV1）。

## 対応表

| 区分 | 元の表記 | 画面の言葉 | API の名前 | 実装側の項目 |
| --- | --- | --- | --- | --- |
| 必ず置き換え | SRT 遅延、受信側の再生待ち | 遅延（「標準（約 10 秒）」「低遅延（約 1.5 秒）」）。送信機に入った映像が受信側に映るまでの時間。運用画面の詳細では「遅延の設定」 | `delay.profile`（`standard` / `low`）, `delay_target_ms`, `DelayProfile` | 送受信の `latency`（`SRTO_LATENCY`）と再生側の待ち時間の組。標準は latency 8000 ms・再生まで 9.5 s（never-drop）、低遅延は約 1.45 s |
| 必ず置き換え | Caller | 送信機から接続（選択肢が 1 つのときは「この送信機から受信機へ接続します」） | `outbound` | caller モード |
| 必ず置き換え | Listener | 接続を待つ（状態の表示は「待ち受け中」） | `inbound` | listener モード |
| 必ず置き換え | Rendezvous | 互いに接続 | `mutual` | rendezvous モード |
| 必ず置き換え | SRT 接続 | 接続、接続方法 | `transport`（`/transport/*`）, `mode` | SRT の接続 |
| 必ず置き換え | SRT 送信（信号経路の段） | 送出（文の中では「送り出し」） | — | SRT の送信処理 |
| 必ず置き換え | SRT 受信（信号経路の段） | 受信 | — | SRT の受信処理 |
| 必ず置き換え | 文中の「SRT で」「SRT の」 | 削る | — | — |
| 必ず置き換え | SRT のエラー名（例：`SRT_REJ_BADSECRET`） | 「パスフレーズの不一致」のように理由を文で | `peer_rejected_passphrase` など | 拒否理由コード |
| 置き換えを推奨 | パスフレーズの「10〜79 文字」 | 範囲外の入力時だけ表示 | `FieldError`（`too_short` / `too_long`、`min` / `max`） | passphrase の長さ制限 |
| 置き換えを推奨 | 映像ビットレート（目標・実際） | 画質（必要なら「画質（ビットレート）」）。「目標 3.5 Mbps／実際 3.3 Mbps」 | `target_mbps`, `actual_mbps`, `EncodeParams.bitrate_mbps` | エンコーダの目標ビットレート、出力の実測 |
| 置き換えを推奨 | 自動 / 手動（適応ビットレート制御） | おまかせ（おすすめ）／画質を固定 | `encode.mode`（`auto` / `manual`） | 適応ビットレート制御のオン・オフ |
| 置き換えを推奨 | 送信側の未送信の映像が溜まり、再生に間に合わなくなるまでの時間 | 映像が止まるまでの余裕（通常は約 6 秒。警告の基準は機器が返す） | `delay_margin_s`, `delay_margin_warn_s`, 警告 `delay_margin_low` | never-drop 制御の残り予算（`nd_budget` = D0 − H） |
| 置き換えを推奨 | 送り出しの遅れによる画質の引き下げ | 送りきれない映像が送信機に溜まってきたため、画質を…に下げました | `bitrate_changed` の `reason: delay` | never-drop 制御の判断（nd-cap / nd-budget など） |
| 置き換えを推奨 | 再生の停止（フレームが来ない） | 映像が止まった（回数と合計秒数） | `stalls_5m`, `stall_s_5m`, 推移 `stall_s`, 警告 `video_stalled` | 再生側の停止検出 |
| 置き換えを推奨 | 推定帯域（SRT による推定） | 回線の推定容量（回線が混んでいるときだけ測れる） | `bandwidth_mbps` | 送信待ちが溜まっているときの ACK レート（`nd_c`）。`mbpsBandwidth` は使わない |
| 置き換えを推奨 | パケット損失 | 回線の損失（「再送で補っています」を添える） | `loss_pct` | 送信機は再送 ÷ 送信（`pktRetrans` / `pktSent`）、受信機は `pktRcvLoss` |
| 置き換えを推奨 | 送信ビットレート | 送っているデータ量（「1 秒あたり。送り直した分を含む」を添える） | `send_mbps` | 送信レート（再送を含む） |
| 必ず置き換え | MAXBW（`SRTO_MAXBW`、送信の最大帯域） | 送り出しの上限（「映像を送る速さの上限です。…伝送中に回線が混むと、この値から自動で下げます。」）。決め方は「（初期値）」「（11/04 14:50 JST に測った値）」「（手で決めた値）」。オプション名は画面にも API にも出さない | `send_limit`（`mbps`、`source`：`default` / `measured` / `manual`、`measured_at`、`test_id`）、`POST /network-tests/{id}/use`、影響 `takes_effect_next_start`、イベント `send_limit_changed` | 送信プログラムの 7 番目の引数（標準のプロファイルの `{maxbw_kbps}` = mbps × 1000）。bufctrl の MAXBW ペーシング（`BUF_ND_PACE`）は送信側の `mbpsMaxBW` を基準にし、回線が混むとそれより下げて送り、空くと戻す。初期値 5000 kbps。`BUF_ND_MAXBW_KBPS` は使わない |
| 置き換えを推奨 | 受信ビットレート | 受け取っているデータ量（「1 秒あたり」を添える） | `recv_mbps` | 受信レート |
| 置き換えを推奨 | RTT | 回線の往復時間（小さな注記で「RTT」と添えてもよい） | `rtt_ms`, `last_rtt_ms` | `msRTT` |
| 置き換えを推奨 | 送信側で破棄したパケット | 送りきれずに捨てたデータ（直近 5 分） | `send_late_5m` | `pktSndDrop` |
| 置き換えを推奨 | 再送要求の割合、再送の割合 | 送り直して届いた割合 | `retrans_pct`（受信機のみ。送信機は `null`） | `pktRcvRetrans` |
| 置き換えを推奨 | 回復できなかったパケット | 届かなかったデータ（直近 5 分） | `unrecovered_5m` | `pktRcvDrop` |
| 置き換えを推奨 | エンコード / デコード（信号経路の段） | 圧縮 / 復元 | — | エンコーダ / デコーダ |
| 置き換えを推奨 | プレビュー（AV1 を二分岐し、H.265 か H.264 で圧縮し直して WebRTC で配る） | プレビュー。方式と映像形式の名前は出さず、何を映しているかを添える（「受信機へ送っている映像を、圧縮したあとの画質で確認用に表示しています。」）。表示できないときは理由を文で（「ほかの画面でプレビューを見ているため、表示できません」「このブラウザではプレビューを表示できません」） | `/preview/sessions`（`offer` / `answer`）、エラー `not_live`・`too_many_viewers`・`bad_offer`・`no_common_codec`・`preview_unavailable` | 送信機は送出直前の AV1、受信機は表示に合わせた AV1 を写し取り、プレビュー用の子プロセスが復号して再圧縮（H.265 を優先、なければ H.264）。視聴は既定 3 画面まで |
| 置き換えを推奨 | デバイスのパス（`/dev/video0`）、ファイルのフルパス | カメラの名前（「Facecam 4K」）、ファイル名だけ | `Media.label`（画面に出す名前）。`VideoInput.camera` / `file` は値として持つだけ | V4L2 デバイス、メディアフォルダー |
| 必ず置き換え | 帯域測定ツール（iperf3）、疎通確認（ping、ICMP）、TCP / UDP の名前 | 「回線の確認」。画面ではネットワーク画面の「つながりの確認」と「回線の速さ」。ツールとプロトコルの名前は画面にも API にも出さない（`npm run check:terms` は iperf を拒否する） | `/network-tests`、`kind`（`reachability` / `throughput`） | 送信機の daemon が iperf3 クライアント（`--json-stream`）、受信機の daemon が iperf3 サーバー（子プロセス、ポート 60022）。疎通は iputils の ping（10 回、0.2 秒おき）。設定は `[network_test]`（`iperf`、`ping`、`port`、`server`、`saturate_mbps`、`hops`） |
| 置き換えを推奨 | 疎通確認（ping） | つながりの確認。状態は「届く／一部届かない／届かない」（● ▲ ■）、「往復時間」、「届いた数（9/10）」 | `kind: reachability`、`targets[]`（`sent`, `received`, `rtt_min_ms` / `rtt_avg_ms` / `rtt_max_ms`） | ping の transmitted / received と rtt min/avg/max |
| 置き換えを推奨 | ホップ（経路上の機器）、WRAN の無線機（Wi-RAN 機材） | 途中の無線機（ネットワーク画面の区切りの名前。一覧では操作する人が付けた名前、例「無線機（送信機側）」）。「ホップ」「WRAN」「Wi-RAN」は画面に出さない | `/settings/network`（`NetworkSettings.hops[]`：`label`, `address`）、`targets[].kind: hop`、影響 `hops_changed`、イベント `network_settings_applied`、入力の誤り `too_many`・`duplicate`・`is_self`・`is_peer` | 初めの値は設定ファイルの `[[network_test.hops]]`（`label`, `address`）。一度適用したら daemon が保存した一覧が優先。daemon は `<state_dir>/link_filter.json` の `hosts` にもこのアドレスを書き、root のヘルパーがそれを読んで WRAN 側のパケットフィルタを作り直す（daemon/README.md） |
| 置き換えを推奨 | パケットフィルタ（WRAN 側で通す通信の限定） | 「無線の回線には、映像とここに書いた無線機との通信だけを流します。」「無線の回線で通信してよい機器」。フィルタ・ファイアウォール・ポートの名前は画面に出さない | — | `link_filter.json`（`peer`, `video_port`, `test_port`, `hosts`）と、それを読む root のヘルパー（このリポジトリの外） |
| 置き換えを推奨 | スループット測定（UDP で回線を飽和させる） | 回線の速さ。「回線が受け止められる以上の量（6 Mbps）を送り、実際に届いた量を測ります」。「飽和」「UDP」「TCP」は画面に出さない。測り方の選択はない（TCP の最大値と UDP の固定レートは 2026-11-04 にやめた） | `kind: throughput`、`direction`、`duration_s`、`NetworkTestOptions.saturate_mbps` | `iperf3 -c <peer> -p <port> -u -b <saturate>M -l 1316 -t <dur> -i 1 --json-stream --forceflush --get-server-output --connect-timeout 3000 [-R]`。`-l 1316` は映像のパケットの大きさ。制御の接続は同じポートの TCP。`saturate` は設定 `network_test.saturate_mbps`（既定 6.0、2〜50） |
| 置き換えを推奨 | 上り / 下り、リバースモード | 向き「送信機 → 受信機（映像の向き）」／「受信機 → 送信機」 | `direction`（`to_peer` / `from_peer`） | 通常 / `-R` |
| 置き換えを推奨 | 受信レート（UDP の received rate）、送信レート、パケット損失、ジッター（回線の速さの結果） | 届いた速さ（小数 2 桁、「5.26 Mbps」。ほぼすべて届いたときは「6 Mbps 以上」）、送った速さ、届かなかった割合（「回線が受け止められる以上の量を送ったので、あふれた分は届きません」を添える）。揺らぎは画面に出さない | `throughput_mbps`, `sent_mbps`, `loss_pct`, `jitter_ms`, `saturated`（`loss_pct` ≥ 1）, `per_second_mbps` | iperf3 の受信側の `end.sum_received.bits_per_second`、送信側の bits_per_second、lost_percent、jitter_ms。1 秒ごとの値は受信側のもの（`to_peer` は `--get-server-output` のサーバーの interval、`-R` は自分の interval） |
| 置き換えを推奨 | 接続タイムアウト、接続拒否、サーバー使用中（回線の速さの失敗） | 「受信機に届きませんでした。つながりの確認を試してください。」「受信機が別の測定をしています。…」「測れませんでした。」 | `error`（`peer_unreachable` / `peer_busy` / `failed`） | iperf3 のエラー文（開発者向けログだけ） |
| そのまま | 映像の遅れ（入力から表示まで） | 映像の遅れ。実測するまでは「映像の遅れ（目安）」として遅延の目標を出す | `video_latency_ms`（未測定は `null`）, `delay_target_ms` | 表示時刻 − 入力時刻 |
| そのまま | パスフレーズ | パスフレーズ（暗号化が「なし」の間は出さない） | `passphrase`（書き込みのみ） | `passphrase` |
| そのまま | 暗号化方式（AES-128 / 192 / 256） | そのまま。選択肢が「なし」だけの間は、比較の表も含めて「暗号化　なし」 | `cipher` | `pbkeylen` |

## 注意点

- **画面に出るログとエラー文**：「ログ」画面やイベント一覧も対象。エラー名は出さず、理由を文で示す。
- **そのほかの表示**：URL、書き出す設定ファイルの名前や項目名、機器情報のバージョン表示にも含めない。ストリーム ID を画面に出す場合は「接続名」とする。
- **ライセンス表示**：libsrt（MPL 2.0）を使うと、ライセンス表示に名前が必要になる。画面の用語とは分けて、表示の場所を決める（`docs/architecture.md`）。
- **確認方法**：`web/` で `npm run check:terms` を実行すると、UI のソースとビルド結果に実装の名前が混ざっていないかを調べる。
