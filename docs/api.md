# API の概要

REST の詳細は `api/openapi.yaml`、型は `web/src/api/types.ts` です。ここでは全体の決まりと、OpenAPI に書けない WebSocket をまとめます。

## 決まり

- すべて `/api/v1` の下。画面と同じオリジンから配る（CORS なし）。
- 名前は `docs/terms.md` の言葉にそろえる。実装の名前（プロトコル名、ライブラリ名、ソケットオプション名、エラー名）を出さない。
- API は **コードと数値** を返し、日本語の文は画面側（`web/src/i18n/ja.ts`）で作る。
- 時刻は ISO 8601 で、機器の時刻帯（`+09:00`）を付ける。画面は機器の時刻を「JST」付きで表示する。
- その役割で意味のない値、いま取れない値は `null`（例：受信機の `send_mbps`、切断中の `rtt_ms`、回線が空いているときの `bandwidth_mbps`）。
- 設定は `version` で楽観ロックする。読んだあとで変わっていたら `409 version_conflict`。
- パスフレーズは書き込み専用。読み出しでは `passphrase_set` だけを返す（暗号化はまだないので、いまは使わない）。
- 選択肢は機器が返す（`.../options`）。いまできないことは選択肢に入れないか、`available: false` で「準備中」として見せる。選択肢が 1 つしかない項目は、画面では操作部品を出さずに文字で示す。

## 主なエンドポイント

| メソッド | パス | 用途 |
| --- | --- | --- |
| GET | `/device` | 機器名・役割（tx / rx）・IP など |
| GET | `/status` | 接続の状態、警告、プレビューの中身 |
| GET | `/stats` | 最新の計測値 |
| GET | `/stats/history?window_s=300` | 直近 5 分の推移（5 秒ごと 60 点） |
| GET | `/events?limit=20` | イベント（新しい順） |
| GET | `/preview.mjpeg` | プレビュー映像（約 5 fps） |
| POST | `/transport/start` ・ `/stop` ・ `/retry` | 開始・停止・今すぐ再試行 |
| GET / PUT | `/settings/video` | 映像設定（送信機）：入力と画質の決め方（おまかせ／画質を固定） |
| POST | `/settings/video/check` | 下書きの検証と、適用したときの影響 |
| GET | `/settings/video/options` | 選べるカメラ・ファイル、画質の範囲（1.5〜4.0 Mbps）、固定の映像形式（`encoding`）、繰り返し再生の可否 |
| GET / PUT | `/settings/transport` | 伝送設定（送信機）：接続先、遅延、暗号化 |
| POST | `/settings/transport/check` | 下書きの検証と影響 |
| GET | `/settings/transport/options` | 接続方法・暗号化・遅延の選択肢（いまは接続方法 `outbound`、暗号化 `none`、遅延は標準のみ。低遅延は `available: false`） |
| GET | `/transport/compare` | 送信機と受信機の設定の比較 |
| POST / GET | `/network-tests`, `/network-tests/{id}` | 回線テスト |

## 設定変更の流れ

1. 画面は `GET /settings/...` の値を「適用中」として表示し、編集はブラウザ内の下書きにする（画面を移っても残る）。
2. 下書きが変わるたびに `POST .../check` を呼ぶ（200 ms 待ってから）。返ってきた `errors` を入力欄に、`impacts` を「適用したときの影響」に出す。影響は回線の様子でも変わるので、接続の状態、おまかせの目標、推定容量が測れるかどうかが変わったときと、確認ダイアログを開いたときにも呼び直す。
3. 「内容を確認して適用…」で確認ダイアログを開き、「適用する」で `PUT`。機器は切り替えが終わってから応答する。
   - 画質（ビットレート）と画質の決め方：映像を途切れさせずに数秒で切り替える（`switch_no_gap`）。
   - 入力の変更：接続をやり直す（`input_switch_gap` と `reconnect_gap`、どちらも `gap_s`。受信側の映像が約 10 秒途切れる）。同じ途切れなので、画面では 1 行にまとめる。
   - 伝送設定：接続をやり直す（`reconnect_gap`。`gap_s` は新しい遅延の設定でおよそ決まる）。
4. `409 version_conflict` なら最新の値を読み直して知らせる。`400 invalid` は check と同じ `errors` を返す。

## 状態（`Status.state`）

| 値 | 画面 | 送信機 | 受信機 |
| --- | --- | --- | --- |
| `live` | ● 伝送中 / 受信中 ＋ 経過時間 | 送っている | 受けている |
| `connecting` | ○ 接続中 | 最初の接続中 | — |
| `reconnecting` | ▲ 再接続中 ＋ n 回目 | 切れて再試行中（`attempt`、`next_retry_s`） | — |
| `listening` | ○ 待ち受け中 ＋ ポート | — | 送信機を待っている |
| `stopped` | ○ 停止中 ＋ 時刻から | 操作で停止。再試行しない | 操作で停止。出力は黒 |

警告（`alerts`）は状態と別に持つ。警告とエラーは自動では消さず、原因がなくなったときに消える。

## 計測値（`Stats`）

1 秒ごと。役割に関係ない値と、いま取れない値は `null`。

| 項目 | 送信機 | 受信機 | 画面の言葉 |
| --- | --- | --- | --- |
| `target_mbps` / `actual_mbps` | 目標と実際の映像ビットレート（実際は直近 6 秒の平均） | — | 画質　目標 3.5 Mbps／実際 3.3 Mbps |
| `send_mbps` | 回線に出している量（再送分を含む、直近 6 秒の平均）。回線が混んでいる間は推定容量くらいになる | — | 送っているデータ量（直近 6 秒の平均。送り直した分を含む） |
| `recv_mbps` | — | 受けている量（直近 6 秒の平均） | 受け取っているデータ量（直近 6 秒の平均） |
| `loss_pct` | 再送したパケット ÷ 送ったパケット（直近 6 秒） | 失われたパケット ÷ 届くはずのパケット（直近 6 秒） | 回線の損失（再送で補っています） |
| `loss_threshold_pct` | 標準の遅延では `null`（損失は再送で補うので警告しない） | 同じ | — |
| `retrans_pct` | `null` | 再送で届いたパケットの割合 | 送り直して届いた割合 |
| `rtt_ms` / `last_rtt_ms` | 往復時間 / 切断前の最後の値 | 同じ | 回線の往復時間 |
| `bandwidth_mbps` | 回線の推定容量。回線が混んでいるときだけ測れる（それ以外と、送り出しの上限で頭打ちのときは `null`） | — | 回線の推定容量 |
| `delay_margin_s` | 受信側の映像が止まるまでの余裕（通常は約 6 秒） | — | 映像が止まるまでの余裕 |
| `delay_margin_warn_s` | 余裕がこれを下回ると `delay_margin_low` を出す（標準の遅延では 3 秒）。推移の破線もこの値 | `null` | 破線の 3 秒を下回ると警告します |
| `send_late_5m` | 送りきれずに捨てたパケット（直近 5 分） | — | 送りきれずに捨てたデータ（直近 5 分） |
| `stalls_5m` / `stall_s_5m` | — | 映像が止まった回数と合計秒数（直近 5 分） | 直近 5 分で映像が止まった回数 |
| `video_latency_ms` | — | 入力から表示までの実測。測れるまでは `null` | 映像の遅れ（実測するまでは「目安」として `delay_target_ms` を表示） |
| `unrecovered_5m` | — | 回復できなかったパケット（直近 5 分） | 届かなかったデータ（直近 5 分） |
| `delay_profile` / `delay_target_ms` | 使っている遅延と、その目標（例 9500 ms） | 同じ | 遅延の設定　標準（約 10 秒） |

推移（`/stats/history` の `series`）は `send_mbps`、`recv_mbps`、`target_mbps`、`actual_mbps`、`loss_pct`、`rtt_ms`、`bandwidth_mbps`、`delay_margin_s`（送信機）、`stall_s`（受信機。5 秒ごとの停止秒数）。

## WebSocket `/api/v1/live`

サーバーからブラウザへの一方向。メッセージは JSON で、型は `LiveMessage`。

| type | いつ | 中身 |
| --- | --- | --- |
| `hello` | 接続した直後 | `device`, `status`, `stats` |
| `status` | 状態が変わったとき＋1 秒ごと | `status` |
| `stats` | 1 秒ごと | `stats` |
| `event` | イベントが起きたとき | `event` |
| `settings` | 設定が適用されたとき | `section`（`video` / `transport`）, `version` |

切れたら画面は 0.5 秒から 10 秒まで間隔を延ばしながらつなぎ直す。推移のグラフは 5 秒ごとに `/stats/history` を読み直す。

## コード

### 警告（`Alert.code`）

| コード | レベル | 画面の文（例） |
| --- | --- | --- |
| `delay_margin_low` | warn | 回線が混み合い、映像が止まるまでの余裕が 2.1 秒になっています。このまま続くと受信側で映像が止まります。おまかせでは自動で画質を下げます。画質を固定しているときは、映像設定で画質を下げてください。（送信機、`margin_s`。`Stats.delay_margin_s` と同じ丸め） |
| `video_stalled` | warn | 直近 5 分で映像が 2 回止まりました（合計 1.3 秒）。続くときは、送信機の映像設定で画質を下げてください。（受信機、`count`・`seconds`・`window_s`） |
| `loss_high` | warn | 回線の損失が多く、映像が乱れるおそれがあります（4.2%）。標準の遅延では出さない（損失は再送で補う） |
| `peer_rejected_passphrase` | error | 受信機 RX-02 が接続を拒否しています。パスフレーズが一致していません。（暗号化ができたときのために残す） |
| `peer_unreachable` | error | 受信機 RX-02 に接続できません。ケーブルと受信機の電源を確認してください。（受信機で出すときは「送信機」） |
| `input_lost` | error | 入力信号を検出できません。カメラの接続を確認してください。 |

### 影響（`Impact.code`）

| コード | レベル | 文 |
| --- | --- | --- |
| `input_switch_gap` | warn | 入力を切り替えて接続をやり直すため、受信側の映像が約 10 秒途切れます（`gap_s`。`reconnect_gap` と一緒に出るが、画面ではこの 1 行だけを出す） |
| `switch_no_gap` | ok | 映像は途切れず、数秒で新しい画質に切り替わります。 |
| `manual_fixed` | info | 画質を固定すると、回線が混んでも自動では下げません |
| `auto_decides` | info | 画質（ビットレート）は回線に合わせて自動で調整します。解像度とフレームレートは入力のままです。 |
| `bitrate_lower_quality` | info | 画質を下げると、動きの多い場面で映像が粗くなることがあります |
| `bitrate_near_bandwidth` | warn | 回線の推定容量は 2.8 Mbps です。固定する画質がこれに近いか上回っているため、映像が止まるおそれがあります。画質を下げるか「おまかせ」を選んでください（固定する画質が推定容量の 0.8 倍を超えるとき。推定容量が測れていないときは出ない） |
| `reconnect` | info | 接続をやり直します |
| `reconnect_gap` | warn | 接続をやり直すため、受信側の映像が約 10 秒途切れます（`gap_s`。ないときは「しばらく」） |
| `no_peer_effect` | ok | いまは未接続のため、受信側への追加の影響はありません |
| `saved_while_stopped` | info | 設定を保存します。伝送は停止したままです |

### イベント（`DeviceEvent.code`）

文の対応は `web/src/i18n/ja.ts` の `eventText`。画質の変更（`bitrate_changed`）は `mbps`・`from_mbps`・`reason` を持ち、理由で文が変わる。

| `reason` | 文（例） |
| --- | --- |
| `loss` | 回線の混雑で損失が増えたため、画質を 3.5 → 2.5 Mbps に下げました |
| `delay` | 送りきれない映像が送信機に溜まってきたため、画質を 3.5 → 2.5 Mbps に下げました |
| `recovered` | 回線に余裕が戻ったため、画質を 2.5 → 3.0 Mbps に上げました |
| `manual` | 画質を 3.0 → 2.5 Mbps に変更しました |

ほかに `delay_margin_low`（`margin_s`）、`video_stalled`（`seconds`：その停止の長さ）、`loss_high`（`loss_pct`）など。

## モック（開発用）

`web/mock/server.ts` がこの API をまねる。開発用に `POST /mock/role {"role":"tx"|"rx"}` と `POST /mock/scenario {"scenario":"normal"|"warn"|"error"}` がある。機器のデーモンには実装しない。

| 状況 | 送信機 | 受信機 |
| --- | --- | --- |
| `normal` | ときどき短く混み、おまかせが 4.0 → 3.5 Mbps に下げ、あとで 4.0 に戻す。余裕は約 6.5 秒 | 損失は再送で補い、映像は止まらない |
| `warn` | 回線が混み、余裕が 3 秒を下回って警告。おまかせが 3.5 → 2.5 Mbps に下げ、余裕が戻ると 3.0、3.5 Mbps へ上げる（180 秒で繰り返す） | 映像がときどき止まる（直近 5 分で 2 回） |
| `error` | 受信機に接続できない（`peer_unreachable`）。5 秒ごとに再接続を試みる | 送信機からの接続を待っている |

回線が混んでいる間、送信機は推定容量くらいで送り（溜まった分が余裕を減らす）、推定容量が測れる。実際の映像のビットレートは、目標が変わってから約 3 秒かけて新しい目標に近づく。
