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
| POST / DELETE | `/preview/sessions`, `/preview/sessions/{id}` | プレビュー（ブラウザで見る確認用の映像）を開く・閉じる |
| POST | `/transport/start` ・ `/stop` ・ `/retry` | 開始・停止・今すぐ再試行 |
| GET / PUT | `/settings/video` | 映像設定（送信機）：入力と画質の決め方（おまかせ／画質を固定） |
| POST | `/settings/video/check` | 下書きの検証と、適用したときの影響 |
| GET | `/settings/video/options` | 選べるカメラ・ファイル、画質の範囲（1.5〜4.0 Mbps）、固定の映像形式（`encoding`）、繰り返し再生の可否、おまかせを使えるか（`auto_available`。低遅延のあいだは `false`） |
| GET / PUT | `/settings/transport` | 伝送設定（送信機）：接続先、遅延、送り出しの上限、暗号化。受信機も答えるが、遅延は送信機に合わせる（下の「遅延」） |
| POST | `/settings/transport/check` | 下書きの検証と影響 |
| GET | `/settings/transport/options` | 接続方法・暗号化・遅延の選択肢（いまは接続方法 `outbound`、暗号化 `none`、遅延は標準と低遅延。機器の設定で低遅延を切ると `available: false`） |
| GET | `/transport/compare` | 送信機と受信機の設定の比較（送信機） |
| GET / PUT | `/settings/network` | 途中の無線機（送信機・受信機）：つながりの確認で確かめ、無線の回線で通信してよい機器の一覧（名前とアドレス、4 台まで） |
| POST | `/settings/network/check` | 下書きの検証と影響 |
| GET / PUT | `/settings/output` | 受信機のモニター（受信機）：届いた映像を、受信機につないだモニターに直接映すか（映さない／全画面／ウィンドウ） |
| POST | `/settings/output/check` | 下書きの検証と影響 |
| POST | `/display` | 運用画面の「低遅延表示」：受信機のモニターに映す・映し方を変える・やめる（下書きなし。受信機） |
| POST / GET | `/network-tests` | 回線の確認を始める（つながりの確認・回線の速さ）／直近 20 件の結果（新しい順） |
| GET | `/network-tests/{id}` | 1 件の進み具合と結果 |
| GET | `/network-tests/options` | 回線の速さで送る量（`saturate_mbps`、6 Mbps。機器の設定で決まり、画面からは変えない）と時間の範囲（5〜30 秒） |
| POST | `/network-tests/{id}/use` | 送信機 → 受信機で測った回線の速さを、送り出しの上限にする（送信機） |

## 設定変更の流れ

1. 画面は `GET /settings/...` の値を「適用中」として表示し、編集はブラウザ内の下書きにする（画面を移っても残る）。
2. 下書きが変わるたびに `POST .../check` を呼ぶ（200 ms 待ってから）。返ってきた `errors` を入力欄に、`impacts` を「適用したときの影響」に出す。影響は回線の様子でも変わるので、接続の状態、おまかせの目標、推定容量が測れるかどうかが変わったときと、確認ダイアログを開いたときにも呼び直す。
3. 「内容を確認して適用…」で確認ダイアログを開き、「適用する」で `PUT`。機器は切り替えが終わってから応答する。
   - 画質（ビットレート）と画質の決め方：映像を途切れさせずに数秒で切り替える（`switch_no_gap`）。
   - 入力の変更：接続をやり直す（`input_switch_gap` と `reconnect_gap`、どちらも `gap_s`。受信側の映像が約 10 秒途切れる）。同じ途切れなので、画面では 1 行にまとめる。
   - 伝送設定：接続をやり直す（`reconnect_gap`。`gap_s` は新しい遅延の設定でおよそ決まる。低遅延なら約 2 秒）。遅延を低遅延にするときは `low_delay_effects`、映像設定がおまかせなら `low_delay_fixed` も出す（下の「遅延」）。
   - 送り出しの上限だけの変更：接続はやり直さない。保存して、次に伝送を始めたときから使う。伝送中（接続中・再接続中も）でも適用できる（`takes_effect_next_start`。いまの伝送とその再試行はこれまでの上限のまま）。ほかの項目と一緒に変えたときは、ほかの項目の決まりに従う（機器は伝送中なら `409 busy`）。
   - 途中の無線機：伝送には触れないので、いつでも（伝送中も）適用できる（`hops_changed`）。`PUT` は `ApplyResult` ではなく新しい `NetworkSettings` を返す。
   - 受信機のモニター：伝送には触れないので、いつでも（伝送中も）適用でき、受信機はすぐに映し方を変える（`display_on` / `display_off` / `display_fullscreen_covers`）。`PUT` は新しい `OutputSettings` を返す。
4. `409 version_conflict` なら最新の値を読み直して知らせる。`400 invalid` は check と同じ `errors` を返す。

## プレビュー

運用画面と全画面表示の映像は、ブラウザの WebRTC で機器から受け取る（外部のサーバーは使わず、LAN の中だけでつなぐ）。

- 送信機は **送っている映像**（圧縮したあと。入力より数秒遅れる）、受信機は **出力している映像**（表示に合わせて止まり、少し遅れる）を見せる。
- 開くのは、接続の状態が `live` で映像があり（`Media.format` があり、黒ではない）、タブが前に出ている間だけ。どれかが外れたら、別の画面へ移ったときも含めて閉じる（`DELETE` と接続の終了）。
- 手順：ブラウザが受信専用の offer を作り、候補を集め終えてから（最大 2 秒）`POST /preview/sessions {"offer": "..."}`。`201 {"id", "answer"}` の answer を設定する。候補を後から送り足すこと（trickle）はしない。
- 機器は最初の offer で準備を始めるので、最初の応答には数秒かかることがある。プレビューは伝送とは別に動き、失敗しても伝送には影響しない。
- 接続が切れたとき（`failed`、または `disconnected` が 5 秒続いたとき）と、answer から 15 秒たっても映らないとき（機器も 15 秒でその視聴を外す）、画面は 3 → 6 → 12 → 30 秒と間隔を延ばしてつなぎ直す。

| エラー | いつ | 画面 |
| --- | --- | --- |
| `409 not_live` | 接続の状態が `live` でない | すぐ画面の状態も変わるので、裏でつなぎ直す |
| `429 too_many_viewers` | 見ている画面の数が機器の上限（既定 3）に達した | ほかの画面でプレビューを見ているため、表示できません ＋「もう一度試す」 |
| `400 bad_offer` | offer がない・空・`v=0` で始まらない・64 KB を超える | プレビューを表示できませんでした ＋「もう一度試す」 |
| `400 no_common_codec` | ブラウザが、プレビューの映像形式をどれも再生できない | このブラウザではプレビューを表示できません |
| `503 preview_unavailable` | プレビューが無効、または準備できなかった | プレビューを表示できませんでした ＋「もう一度試す」 |

`DELETE /preview/sessions/{id}` は、知らない id でも `204`。

## 回線の確認（ネットワーク画面）

送信機と受信機の間の回線を、画面から確かめる。2 種類ある（`NetworkTest.kind`）。結果は機器のメモリーに直近 20 件だけ持ち、再起動すると消える。

| `kind` | 画面 | 役割 | いつ | 中身 |
| --- | --- | --- | --- | --- |
| `reachability` | つながりの確認 | 送信機・受信機 | いつでも（伝送中も。とても小さい） | 相手と、途中の無線機（`/settings/network` で適用中の一覧）が応答するか、往復時間。10 回ずつ、約 2〜3 秒 |
| `throughput` | 回線の速さ | 送信機だけ（受信機は `409 unsupported_role`） | 接続の状態が `stopped` のときだけ。1 度に 1 つ | 回線が受け止められる以上の量（`saturate_mbps`、6 Mbps）を送り、実際に届いた量を測る。5〜30 秒（既定 10 秒） |

- 始める：`POST /network-tests {"kind": "reachability"}` または `{"kind": "throughput", "direction", "duration_s"}`。すぐに `202` で、`state: "running"` のテストを返す。画面は `GET /network-tests` を、走っている間は 1 秒ごと、それ以外は 5 秒ごとに読み直す（ほかの画面で始めたテストも出る）。前の版の `mode`・`rate_mbps` は送られても無視する。
- 調べる順番：`kind` がない・知らない値（`400 invalid`、`field: "kind"`）→ 受信機で `throughput`（`409 unsupported_role`）→ 入力の誤り（`400 invalid`。`direction` は `invalid`、`duration_s` 5〜30 の整数は `out_of_range` と `min`・`max`）→ `409 busy`。
- `409 busy`：`throughput` は接続の状態が `stopped` でないとき（測ると映像が止まるため）と、別の `throughput` が走っているとき。`reachability` は別の `reachability` が走っているとき。
- `throughput` が走っている間は、`POST /transport/start` も `409 busy`（画面は「回線の速さを測っているため、伝送を開始できません。…」）。
- `direction`：`to_peer` はこの機器から相手へ（送信機では映像の向き）、`from_peer` は相手からこの機器へ。
- 送る量は回線の容量より多いので、届かない分（`loss_pct`）が出るのはふつう。届いた量（`throughput_mbps`）が回線の速さ。届かない分が 1 % 未満（`saturated: false`）なら、回線は送った量より速いので、画面は「6 Mbps 以上」と出す。
- 受信機は、機器が動いている間いつでも `throughput` の相手になる。伝送とは別に動き、伝送には影響しない。

`NetworkTest`

| 項目 | 中身 |
| --- | --- |
| `id`, `kind`, `state` | `running` → `done` か `failed` |
| `started_at`, `finished_at` | 走っている間 `finished_at` は `null` |
| `progress` | 0〜1 |
| `request` | 機器が実際に使った値（既定値を埋めたもの）：`kind`、`direction`、`duration_s`。`reachability` では `direction`・`duration_s` は `null` |
| `result` | 下の表。走っている間（`progress` だけが進む）と失敗したときは `null` |
| `error` | 失敗したとき：`peer_unreachable`（相手に届かない：経路がない・応答がない・断られた）、`peer_busy`（相手が別の測定をしている）、`failed`（それ以外。詳しいことは開発者向けログだけに出す） |

| 結果 | 項目 | 画面 |
| --- | --- | --- |
| `reachability` | `targets[]`：`kind`（`peer` / `hop`）、`label`（`hop` の名前。`peer` は `null`）、`address`、`sent`、`received`、`rtt_min_ms`・`rtt_avg_ms`・`rtt_max_ms`（届かなければ `null`）。相手が先、そのあと途中の無線機を、テストを始めたときに適用中の一覧の順に。受信機で相手のアドレスがわからない（まだ一度もつながっていない、設定もない）ときは相手を入れない。空のこともある | 相手、アドレス、状態（● 届く ▲ 一部届かない ■ 届かない）、往復時間（平均と最小〜最大）、届いた数（9/10） |
| `throughput` | `throughput_mbps`（届いた速さ）、`sent_mbps`（送った速さ）、`loss_pct`（届かなかった割合）、`jitter_ms`（遅れのばらつき。画面には出さない）、`saturated`（`loss_pct` ≥ 1。回線がいっぱいだった）、`per_second_mbps`（1 秒ごとに届いた量） | 届いた速さ 5.26 Mbps（小数 2 桁）か「6 Mbps 以上」、送った速さ、届かなかった割合 |

終わった `throughput` は、イベント `network_test_done`（`mbps`：`throughput_mbps` と同じ小数 2 桁、`direction`）を出す（「回線テストが終わりました（送信機 → 受信機、届いた速さ 5.26 Mbps）」）。

## 途中の無線機（`/settings/network`）

送信機と受信機の間にある無線機の一覧。両方の機器にあり、それぞれの機器で設定する（2026-11-04 のユーザーの依頼：無線機のアドレスは変わることがある）。

- つながりの確認は、相手のあとにこの一覧の順で確かめる。機器は、無線の回線に流す通信を、映像とこの一覧の機器との通信だけに絞る（機器の側の仕組みは `daemon/README.md`）。
- `GET /settings/network` → `NetworkSettings {version, hops: [{label, address}]}`。まだ一度も適用していないときは、機器の設定ファイルの一覧（`network_test.hops`）を返す。一度適用したら、保存した一覧を返す（再起動しても残る）。
- `POST /settings/network/check`（本文 `NetworkSettingsInput {version, hops}`）→ `CheckResult`。一覧が適用中と違えば影響 `hops_changed`（info）。
- `PUT /settings/network` → `200 NetworkSettings`、`400 invalid`（check と同じ `errors`）、`409 version_conflict`（調べる順も、この順）。どちらの役割でも、伝送中でも受け付ける。適用すると `version` が上がり、イベント `network_settings_applied` と `settings`（`section: "network"`）を出す。

| 項目 | 決まり | エラー（`field`、`code`） |
| --- | --- | --- |
| `hops` | 0〜4 台。送った一覧が、適用中の一覧と丸ごと入れ替わる | `hops`、`too_many`（`max` 4） |
| `hops[i].label` | 前後の空白を除いて 1〜24 文字（機器は除いた形で保存する） | `hops[i].label`、`required` / `too_long`（`max` 24） |
| `hops[i].address` | 点で区切った IPv4 のアドレス（名前は不可）。`0.0.0.0`・`255.255.255.255`・`127.x.x.x` は不可 | `hops[i].address`。当てはまる最初の 1 つだけ：`invalid` → `is_self`（この機器のアドレス）→ `is_peer`（相手のアドレス。送信機では伝送設定の接続先、受信機では設定にある送信機か、最後につながった送信機）→ `duplicate`（前の行と同じアドレス。後ろの行に付く） |

`i` は送った一覧の中の位置（0 から）。画面は、空の欄、名前を書いた欄、使えないアドレスを、入力された値で言い分ける（機器はどれも `invalid`）。

## 受信機のモニター（`/settings/output`、`/display`）

受信機が、届いた映像を受信機につないだモニターに直接映す（2026-11-04 のユーザーの依頼：ブラウザのプレビューは、待ち合わせ・圧縮し直し・ブラウザの分だけ遅れる。直接なら約 0.5 秒早く、なめらかに映る）。受信機だけ。送信機は、どれも `409 unsupported_role`。

- `GET /settings/output` → `OutputSettings {version, display}`。`display` は `off`（映さない。初期値、`version` 1）／ `fullscreen`（全画面。モニターには映像だけが映る）／ `window`（受信機の画面のウィンドウ）。再起動しても残る。
- `POST /settings/output/check`（本文 `OutputSettingsInput {version, display}`）→ `CheckResult`。値が適用中と違うときだけ影響を返す：`display_on`（`fullscreen` か `window` にする）か `display_off`（`off` にする）、新しい値が `fullscreen` なら続けて `display_fullscreen_covers`（どれも info）。
- `PUT /settings/output` → `200 OutputSettings`。調べる順：本文が JSON でない（`400 invalid`、`errors` は空）→ `version` が違う（`409 version_conflict`）→ 値が 3 つのどれでもない（`400 invalid`、`field: "display"`、`invalid`）。
- `POST /display`（本文 `{"mode": "off" | "fullscreen" | "window"}`）→ `200 OutputSettings`。運用画面のボタン（「低遅延表示」は `window`、「全画面にする」「ウィンドウにする」、「ブラウザ表示に戻す」は `off`）。設定の下書きではなく操作なので、`version` も check もない。値が 3 つのどれでもなければ `400 invalid`（`field: "mode"`）。
- どちらで変えても同じ値が変わる：`version` が上がり（同じ値でも。受信機はもう一度映し直す）、保存し、イベント `output_settings_applied`（`display`）と `settings`（`section: "output"`）を出す。伝送中でも、接続の状態がどれでも受け付け（`busy` はない）、受信機がすぐに映し方を変えてから応答する。受信を止めているあいだは、受信を始めたときから映す。
- 受信機の様子は `Stats.display`：`on`（全画面かウィンドウで映している）、`off`、`failed`（映せなかった。受信機の画面にだれもログインしていないときなど）。受信を止めているあいだと、受信機がまだ知らせていないあいだは `null`。送信機は `null`。`failed` のあいだ、設定が `off` でなければ、受信機は 30 秒ごとにもう一度映してみる。
- 画面（運用）：受信機のモニターに映している（`Stats.display` が `on`）か、設定が `off` でなく受信機がまだ知らせていないあいだは、ブラウザのプレビューを開かない（受信機で二重に復元しない、遅れを増やさない）。「低遅延表示」を押すと、先にプレビューを閉じてから `POST /display` を送る。設定画面（映像設定）は、ほかの設定と同じ下書き → 確認 → 適用の流れ。

## 遅延（`TransportSettings.delay`）

送信機で「標準（約 10 秒）」か「低遅延（約 1.5 秒）」を選ぶ（`DelayProfile`：`standard` / `low`）。受信機は、つながった送信機の遅延に自動で合わせる（2026-11-04 のユーザーの依頼）。

- 選んだ遅延は、次に伝送を始めたときから使う（伝送中の変更は接続のやり直し。再試行と再接続は、始めたときの遅延のまま）。
- **低遅延のあいだの画質**（2026-11-04 の決定。戻せる）：映像設定は書き換えない。伝送設定の遅延が低遅延のあいだは、映像設定の `encode.mode` が何であっても、固定の画質 `encode.manual.bitrate_mbps`（初期値 3.0 Mbps）で送る。標準に戻すと、保存してある決め方（おまかせなど）に戻る。
  - `VideoOptions.auto_available`：低遅延のあいだ `false`（それ以外と受信機では `true`）。画面はおまかせを押せなくする。
  - 映像設定の check：低遅延のあいだに `mode: auto` → `low_delay_fixed {bitrate_mbps}`（info。誤りではない。保存はでき、標準に戻すと使う）。低遅延のあいだは決め方だけの変更では送るものが変わらないので、`switch_no_gap` と `auto_decides` は出さない。
  - 伝送設定の check：遅延を低遅延にする下書きで、適用中の映像設定がおまかせなら `low_delay_fixed {bitrate_mbps}`、いつでも `low_delay_effects`（どちらも info。状態による影響のあと）。伝送中なら、いつもどおり `reconnect_gap`（新しい遅延の `gap_s`）。
- 低遅延では、映像が止まるまでの余裕と、その警告がない：送信機の `Stats.delay_margin_s` と `delay_margin_warn_s` は `null`、`delay_margin_low` は出ない。
- **受信機は送信機に合わせる**：受信機の `TransportSettings.delay` は `{profile, follows_peer: true}`。`profile` はいまの接続か最後の接続の送信機の遅延（まだ一度もつながっていなければ `standard`。機器のメモリーだけに持つ）。受信機の伝送設定の check と `PUT` は `delay` を無視する（誤りにも変更にもならない）。送信機では `follows_peer: false`。受信機の伝送設定の画面はまだデザインしていない。
- 受信機の `Stats.delay_profile` は、いまの接続か最後の接続の送信機の遅延。`delay_target_ms` はその目標に、送信機がつながってから受信機が自分で足した待ち時間（フレームが遅れて届くと少し長く待つ）を加えたもの（100 ms 単位）。
- 機器の設定で低遅延を切ると（`available: false`）、低遅延の `PUT` は `delay.profile: invalid` で断られる。

## 送り出しの上限（`TransportSettings.send_limit`）

送信機が回線に出す量の上限。伝送を始めるときにこの値で始め、回線が混むと機器が自分でこれより下げて送り、空けばこの値まで戻す。初期値は 5.0 Mbps。

| 項目 | 中身 |
| --- | --- |
| `mbps` | 1.0〜20.0、小数 2 桁 |
| `source` | `default`（初期値）、`measured`（回線の速さを測った値）、`manual`（伝送設定で入力した値） |
| `measured_at`, `test_id` | `measured` のとき、そのテストの `finished_at` と `id`。それ以外は `null` |

- 機器の設定ファイルに保存し、再起動しても残る。変えると伝送設定の `version` が上がり、`settings`（`section: "transport"`）を送る。
- 伝送設定で変える：`PUT /settings/transport` の `send_limit: {"mbps"}`（省くといまの値のまま）。範囲外は `field: "send_limit.mbps"`、`out_of_range`（`min` 1、`max` 20）。新しい値は `source: "manual"`、同じ値なら元の `source` のまま。停止中以外でこれだけを変えるときは断らず、影響 `takes_effect_next_start` を返す。
- 測った値にする：`POST /network-tests/{id}/use`（本文なし）。`done` の `throughput` で `direction: "to_peer"` のものだけ。`throughput_mbps` を小数 2 桁に丸めた値（`saturated: false` なら `saturate_mbps`。1.0〜20.0 に収める）を `source: "measured"` で入れ、新しい `TransportSettings` を `200` で返す。伝送中でも受け付け、次に伝送を始めたときから使う。
  - 調べる順番：受信機（`409 unsupported_role`）→ 知らない `id`（`404 not_found`）→ `throughput` でない・終わっていない・`to_peer` でない（`409 not_applicable`）。
- どちらで変えても、イベント `send_limit_changed`（`mbps`、`from_mbps`、`source`）を出す（「送り出しの上限を 5.00 → 5.26 Mbps にしました（回線の速さを測った値）」「送り出しの上限を 5.26 → 4.50 Mbps に変更しました」）。
- 動いている伝送の上限は `Status`・`Stats` には出さない（`bandwidth_mbps` が `null` のときの「送り出しの上限で頭打ち」で間接に分かる）。

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
| `target_mbps` / `actual_mbps` | 目標と実際の映像ビットレート（実際はキーフレーム 3 つ分、約 6 秒の平均） | — | 画質　目標 3.5 Mbps／実際 3.3 Mbps |
| `send_mbps` | 回線に出している量（再送分を含む、直近 6 秒の平均）。回線が混んでいる間は推定容量くらいになる | — | 送っているデータ量（直近 6 秒の平均。送り直した分を含む） |
| `recv_mbps` | — | 受けている量（直近 6 秒の平均） | 受け取っているデータ量（直近 6 秒の平均） |
| `loss_pct` | 再送したパケット ÷ 送ったパケット（直近 6 秒） | 失われたパケット ÷ 届くはずのパケット（直近 6 秒） | 回線の損失（再送で補っています） |
| `loss_threshold_pct` | 標準の遅延では `null`（損失は再送で補うので警告しない） | 同じ | — |
| `retrans_pct` | `null` | 再送で届いたパケットの割合 | 送り直して届いた割合 |
| `rtt_ms` / `last_rtt_ms` | 往復時間 / 切断前の最後の値 | 同じ | 回線の往復時間 |
| `bandwidth_mbps` | 回線の推定容量。回線が混んでいるときだけ測れる（それ以外と、送り出しの上限で頭打ちのときは `null`） | — | 回線の推定容量 |
| `delay_margin_s` | 受信側の映像が止まるまでの余裕（通常は約 6 秒）。低遅延では `null` | — | 映像が止まるまでの余裕（低遅延では「低遅延のあいだは測りません」） |
| `delay_margin_warn_s` | 余裕がこれを下回ると `delay_margin_low` を出す（標準の遅延では 3 秒）。推移の破線もこの値。低遅延では `null` | `null` | 破線の 3 秒を下回ると警告します |
| `send_late_5m` | 送りきれずに捨てたパケット（直近 5 分） | — | 送りきれずに捨てたデータ（直近 5 分） |
| `stalls_5m` / `stall_s_5m` | — | 映像が止まった回数と合計秒数（直近 5 分） | 直近 5 分で映像が止まった回数 |
| `video_latency_ms` | — | 入力から表示までの実測。測れるまでは `null` | 映像の遅れ（実測するまでは「目安」として `delay_target_ms` を表示） |
| `unrecovered_5m` | — | 回復できなかったパケット（直近 5 分） | 届かなかったデータ（直近 5 分） |
| `delay_profile` / `delay_target_ms` | 使っている遅延と、その目標（例 9500 ms）。停止中は次の伝送で使う遅延 | いまの接続か最後の接続の送信機の遅延。目標には受信機が自分で足した待ち時間を含む（100 ms 単位） | 遅延の設定　標準（約 10 秒）（受信機では「送信機の設定に合わせています」を添える） |
| `display` | `null` | 受信機のモニターの様子：`on`・`off`・`failed`。受信を止めているあいだと、まだ知らせていないあいだは `null` | 受信機のモニター　● 映しています（全画面）／○ 映していません／▲ 映せませんでした |

推移（`/stats/history` の `series`）は `send_mbps`、`recv_mbps`、`target_mbps`、`actual_mbps`、`loss_pct`、`rtt_ms`、`bandwidth_mbps`、`delay_margin_s`（送信機）、`stall_s`（受信機。5 秒ごとの停止秒数）。

## WebSocket `/api/v1/live`

サーバーからブラウザへの一方向。メッセージは JSON で、型は `LiveMessage`。

| type | いつ | 中身 |
| --- | --- | --- |
| `hello` | 接続した直後 | `device`, `status`, `stats` |
| `status` | 状態が変わったとき＋1 秒ごと | `status` |
| `stats` | 1 秒ごと | `stats` |
| `event` | イベントが起きたとき | `event` |
| `settings` | 設定が適用されたとき | `section`（`video` / `transport` / `network` / `output`）, `version` |

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
| `takes_effect_next_start` | info | 送り出しの上限は、次に伝送を始めたときから使います。いまの伝送は、これまでの上限のまま続けます（停止中以外で、送り出しの上限だけを変えるとき） |
| `hops_changed` | info | つながりの確認で確かめる機器と、無線の回線で通信してよい機器が、この内容に変わります。（途中の無線機の一覧が適用中と違うとき） |
| `low_delay_fixed` | info | 低遅延のあいだは、おまかせは使えません。画質は固定の 3.0 Mbps で送ります（映像設定の「固定する画質」の値）。（`bitrate_mbps`。伝送設定：遅延を低遅延にし、映像設定がおまかせのとき。映像設定：低遅延のあいだにおまかせのとき） |
| `low_delay_effects` | info | 動きの多い場面や回線が混んだときは、受信側の遅れが少しずつ増えることがあります。遅れは、回線が落ち着くと 1 分に 1 コマずつ戻ります。（伝送設定：遅延を低遅延にするとき） |
| `display_on` | info | 受信機のモニターに、すぐに映します。（受信機のモニターを全画面かウィンドウにするとき。受信を止めているあいだは、画面が「受信を始めると、受信機のモニターに映します。」と出す） |
| `display_off` | info | 受信機のモニターの映像を消します。（映さないにするとき。止めているあいだは「受信を始めても、受信機のモニターには映しません。」） |
| `display_fullscreen_covers` | info | 全画面のあいだは、受信機のモニターでこの画面を見られません。ほかのパソコンやタブレットから操作してください。（全画面にするとき） |

### イベント（`DeviceEvent.code`）

文の対応は `web/src/i18n/ja.ts` の `eventText`。画質の変更（`bitrate_changed`）は `mbps`・`from_mbps`・`reason` を持ち、理由で文が変わる。

| `reason` | 文（例） |
| --- | --- |
| `loss` | 回線の混雑で損失が増えたため、画質を 3.5 → 2.5 Mbps に下げました |
| `delay` | 送りきれない映像が送信機に溜まってきたため、画質を 3.5 → 2.5 Mbps に下げました |
| `recovered` | 回線に余裕が戻ったため、画質を 2.5 → 3.0 Mbps に上げました |
| `manual` | 画質を 3.0 → 2.5 Mbps に変更しました |

ほかに `delay_margin_low`（`margin_s`）、`video_stalled`（`seconds`：その停止の長さ）、`loss_high`（`loss_pct`）、`network_test_done`（`mbps`、`direction`）、`send_limit_changed`（`mbps`、`from_mbps`、`source`：`measured` / `manual`）、`network_settings_applied`（途中の無線機を適用した。「途中の無線機の設定を適用しました」）、`output_settings_applied`（受信機のモニターを変えた。`display`。「受信機のモニターにウィンドウで映すようにしました」「受信機のモニターに映すのをやめました」）など。

### 入力の誤り（`FieldError.code`）

`out_of_range`（`min`・`max`）、`required`、`too_short` / `too_long`（`min` / `max`）、`invalid`（受信機のモニターの `display`・`mode` も）、`too_many`（一覧が長すぎる。`max`）、`duplicate`・`is_self`・`is_peer`（途中の無線機のアドレス。上の表）。

### エラー（`ApiError.code`）の追加

`not_applicable`（409）：`POST /network-tests/{id}/use` で、そのテストが送り出しの上限に使えない（回線の速さでない、終わっていない、送信機 → 受信機でない）。

## モック（開発用）

`web/mock/server.ts` がこの API をまねる。プレビューの映像はまねられないので、`POST /preview/sessions` は `live` の間 `503 preview_unavailable` を返す（画面には「プレビューを表示できませんでした」が出る）。開発用に `POST /mock/role {"role":"tx"|"rx"}` と `POST /mock/scenario {"scenario":"normal"|"warn"|"error"}` がある。機器のデーモンには実装しない。

| 状況 | 送信機 | 受信機 |
| --- | --- | --- |
| `normal` | ときどき短く混み、おまかせが 4.0 → 3.5 Mbps に下げ、あとで 4.0 に戻す。余裕は約 6.5 秒 | 損失は再送で補い、映像は止まらない |
| `warn` | 回線が混み、余裕が 3 秒を下回って警告。おまかせが 3.5 → 2.5 Mbps に下げ、余裕が戻ると 3.0、3.5 Mbps へ上げる（180 秒で繰り返す） | 映像がときどき止まる（直近 5 分で 2 回） |
| `error` | 受信機に接続できない（`peer_unreachable`）。5 秒ごとに再接続を試みる | 送信機からの接続を待っている |

回線が混んでいる間、送信機は推定容量くらいで送り（溜まった分が余裕を減らす）、推定容量が測れる。実際の映像のビットレートは、目標が変わってから約 3 秒かけて新しい目標に近づく。

遅延は標準と低遅延を選べる。送信機で低遅延を適用すると、固定する画質で送り（おまかせの段階の変更と余裕・その警告はなし、`auto_available: false`）、標準に戻すと保存した決め方に戻る。受信機は送信機に合わせる：モックには送信機がないので、決まった値（`MOCK_PEER_DELAY`、既定 `low`）を計測値と伝送設定（`follows_peer: true`）に出す。

回線の確認は、実際の設置（約 5 Mbps の無線区間、往復約 75 ms、両端の近くに無線機）をまねる。結果はモックを切り替えると消える。

| 状況 | つながりの確認 | 回線の速さ（送信機。6 Mbps を送る） |
| --- | --- | --- |
| `normal` | 相手 約 75 ms、近い無線機 約 0.4 ms、遠い無線機 約 75 ms。すべて 10/10 | 送信機 → 受信機 届いた速さ 5.26 Mbps（届かなかった割合 5.8 %）、受信機 → 送信機 1.15 Mbps |
| `warn` | 遠い無線機と相手が 9/10（▲ 一部届かない） | 3.18 Mbps と 0.86 Mbps |
| `error` | 近い無線機だけ届き、遠い無線機と相手は届かない | 接続を試す 3 秒のあと `peer_unreachable`（送信機は再接続中なので、先に停止が要る） |

途中の無線機は、実際の設置の 2 台（この機器の近くの無線機が先）を `version` 1 で持ち、機器と同じ決まりで調べて保存する（モックを切り替えると元に戻る）。つながりの確認は適用中の一覧を使い、どちらの無線機もいないアドレスは届かない（0/10）。

受信機のモニターは `off`（`version` 1）から始まり、機器と同じ決まりで調べて保存する（モックを切り替えると元に戻る）。`Stats.display` は、止めているあいだ `null`、映さないなら `off`、映すなら `on`。`MOCK_DISPLAY=failed` で起動すると、映すときに `failed`（受信機の画面にログインしていないときのまね）。

`MOCK_LINE_FAST=1` で起動すると、回線が送る量より速い（ほぼすべて届く、`saturated: false`。画面は「6 Mbps 以上」）。送り出しの上限は初期値 5.00 Mbps から始まり、`/use` と伝送設定の適用で機器と同じように変わる（モックを切り替えると初期値に戻る）。
