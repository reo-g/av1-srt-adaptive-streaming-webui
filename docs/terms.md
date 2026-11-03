# 用語の言い換え対応表

画面にも API にも「SRT」の名前や、SRT 独自の言葉は出しません。設定項目は「何をするものか」で名付け、ほかの伝送方式でも使う一般的な用語はそのまま使います。実装の内部（設定ファイルの内部表現、開発者向けログ）では元の用語を使ってかまいません。

元の対応表は Claude の文書「用語の言い換え対応表」です。この表はそれに、API で使う名前を加えたものです。

## 区分

- **必ず置き換える**：「SRT」の文字、SRT 独自の接続モード名（Caller / Listener / Rendezvous）、SRT 独自の設定名（SRT 遅延）。
- **置き換えを推奨**：言葉は一般的でも、組み合わせると SRT だと分かる表現。パスフレーズの文字数「10〜79 文字」など。
- **そのまま使う**：パケット損失、再送率、推定帯域、RTT、暗号化方式（AES-128 など）、伝送、受信機・送信機。

## 対応表

| 区分 | 元の表記 | 画面の言葉 | API の名前 | 実装側の項目 |
| --- | --- | --- | --- | --- |
| 必ず置き換え | SRT 遅延 | 伝送バッファ | `buffer`, `buffer_ms` | `latency`（`SRTO_LATENCY`） |
| 必ず置き換え | Caller | 発信 | `outbound` | caller モード |
| 必ず置き換え | Listener | 待ち受け | `inbound` | listener モード |
| 必ず置き換え | Rendezvous | 相互接続 | `mutual` | rendezvous モード |
| 必ず置き換え | SRT 接続 | 接続 | `transport`（`/transport/*`） | SRT の接続 |
| 必ず置き換え | SRT 送信（信号経路の段） | 送出 | — | SRT の送信処理 |
| 必ず置き換え | SRT 受信（信号経路の段） | 受信 | — | SRT の受信処理 |
| 必ず置き換え | SRT による推定（推定帯域の補足） | 回線から推定 | `bandwidth_mbps` | `mbpsBandwidth` |
| 必ず置き換え | 文中の「SRT で」「SRT の」 | 削る | — | — |
| 必ず置き換え | SRT のエラー名（例：`SRT_REJ_BADSECRET`） | 「パスフレーズの不一致」のように理由を文で | `peer_rejected_passphrase` など | 拒否理由コード |
| 置き換えを推奨 | パスフレーズの「10〜79 文字」 | 範囲外の入力時だけ表示 | `FieldError`（`too_short` / `too_long`、`min` / `max`） | passphrase の長さ制限 |
| 置き換えを推奨 | 送信側で破棄したパケット | 送信が間に合わなかったパケット | `send_late_5m` | `pktSndDrop` |
| 置き換えを推奨 | 再送要求の割合 | 再送の割合 | `retrans_pct`（受信機） | `pktSentNAK` |
| そのまま | パスフレーズ | パスフレーズ | `passphrase`（書き込みのみ） | `passphrase` |
| そのまま | 暗号化方式（AES-128 / 192 / 256） | そのまま | `cipher` | `pbkeylen` |
| そのまま | パケット損失、再送率、回復できなかったパケット | そのまま | `loss_pct`, `retrans_pct`, `unrecovered_5m` | `pktRcvLoss`, `pktRetrans`, `pktRcvDrop` |
| そのまま | RTT | そのまま | `rtt_ms`, `last_rtt_ms` | `msRTT` |
| そのまま | 目安「RTT の 4 倍以上」 | そのまま（伝送バッファが手動のときだけ表示） | `guide_min_ms` | latency の推奨値 |

## 注意点

- **画面に出るログとエラー文**：「ログ」画面やイベント一覧も対象。エラー名は出さず、理由を文で示す。
- **そのほかの表示**：URL、書き出す設定ファイルの名前や項目名、機器情報のバージョン表示にも含めない。ストリーム ID を画面に出す場合は「接続名」とする。
- **ライセンス表示**：libsrt（MPL 2.0）を使うと、ライセンス表示に名前が必要になる。画面の用語とは分けて、表示の場所を決める（`docs/architecture.md`）。
- **確認方法**：`web/` で `npm run check:terms` を実行すると、UI のソースとビルド結果に実装の名前が混ざっていないかを調べる。
