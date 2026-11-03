# API の概要

REST の詳細は `api/openapi.yaml`、型は `web/src/api/types.ts` です。ここでは全体の決まりと、OpenAPI に書けない WebSocket をまとめます。

## 決まり

- すべて `/api/v1` の下。画面と同じオリジンから配る（CORS なし）。
- 名前は `docs/terms.md` の言葉にそろえる。実装の名前（プロトコル名、ライブラリ名、ソケットオプション名、エラー名）を出さない。
- API は **コードと数値** を返し、日本語の文は画面側（`web/src/i18n/ja.ts`）で作る。
- 時刻は ISO 8601 で、機器の時刻帯（`+09:00`）を付ける。画面は機器の時刻を「JST」付きで表示する。
- その役割で意味のない値、いま取れない値は `null`（例：受信機の `send_mbps`、切断中の `rtt_ms`）。
- 設定は `version` で楽観ロックする。読んだあとで変わっていたら `409 version_conflict`。
- パスフレーズは書き込み専用。読み出しでは `passphrase_set` だけを返す。

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
| GET / PUT | `/settings/video` | 映像設定（送信機） |
| POST | `/settings/video/check` | 下書きの検証と、適用したときの影響 |
| GET | `/settings/video/options` | 選べるカメラ・ファイル・範囲 |
| GET / PUT | `/settings/transport` | 伝送設定（送信機） |
| POST | `/settings/transport/check` | 下書きの検証と影響 |
| GET | `/settings/transport/options` | 伝送バッファの選択肢 |
| GET | `/transport/compare` | 送信機と受信機の設定の比較 |
| POST / GET | `/network-tests`, `/network-tests/{id}` | 回線テスト |

## 設定変更の流れ

1. 画面は `GET /settings/...` の値を「適用中」として表示し、編集はブラウザ内の下書きにする（画面を移っても残る）。
2. 下書きが変わるたびに `POST .../check` を呼ぶ（200 ms 待ってから）。返ってきた `errors` を入力欄に、`impacts` を「適用したときの影響」に出す。
3. 「内容を確認して適用…」で確認ダイアログを開き、「適用する」で `PUT`。機器は適用が終わってから応答する（映像はエンコーダの再初期化、伝送は接続のやり直し）。
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
| `loss_high` | warn | 回線のパケット損失が基準を超えています（2.1%）。 |
| `frames_dropped` | warn | 直近 5 分で映像が 3 フレーム欠落しました。（受信機） |
| `peer_rejected_passphrase` | error | 受信機 RX-02 が接続を拒否しています。パスフレーズが一致していません。 |
| `peer_unreachable` | error | 受信機 RX-02 に接続できません。 |
| `input_lost` | error | 入力信号を検出できません。 |

### 影響（`Impact.code`）

| コード | レベル | 文 |
| --- | --- | --- |
| `input_switch_gap` | warn | 入力を切り替える間、受信側の映像が約 1 秒途切れます |
| `encoder_restart_gap` | warn | 伝送中のため、受信側の映像が約 2 秒途切れます（エンコーダの再初期化） |
| `connection_kept` | ok | 接続は維持されます。再接続は起きません |
| `manual_fixed` | info | 手動の値は、入力や回線が変わっても自動では変わりません |
| `auto_decides` | info | 解像度・フレームレート・ビットレートは機器が決めます |
| `bitrate_lower_quality` | info | ビットレートを下げると、動きの多い場面で画質が下がることがあります |
| `bitrate_near_bandwidth` | warn | 推定帯域 52 Mbps に対して余裕が少なくなり、損失が増えるおそれがあります |
| `reconnect` | info | 接続をやり直します |
| `reconnect_gap` | warn | 接続をやり直すため、受信側の映像が数秒途切れます |
| `no_peer_effect` | ok | いまは未接続のため、受信側への追加の影響はありません |
| `saved_while_stopped` | info | 設定を保存します。伝送は停止したままです |
| `buffer_below_guide` | warn | 伝送バッファが目安の 152 ms より短く、損失が増えると映像が乱れやすくなります |

イベントのコードと文の対応は `web/src/i18n/ja.ts` の `eventText` を参照。

## モック（開発用）

`web/mock/server.ts` がこの API をまねる。開発用に `POST /mock/role {"role":"tx"|"rx"}` と `POST /mock/scenario {"scenario":"normal"|"warn"|"error"}` がある。機器のデーモンには実装しない。
