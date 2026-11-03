# 映像伝送機器の Web UI

LAN で使う映像の送信機・受信機を、PC やタブレットのブラウザから操作する画面です。いまは画面と、機器の API をまねるモックがあります。機器側（デーモンと映像処理）はこれから作ります。

## 動かし方

Node.js 20 以上が必要です。

```
cd web
npm install
npm run mock     # 機器の API のモック（:8787）
npm run dev      # 別のターミナルで。http://localhost:5173 を開く
```

- 右下の「モック」で、送信機／受信機と、状況（正常・回線警告・接続エラー）を切り替えられます。
- 起動時に決めるなら `MOCK_ROLE=rx MOCK_SCENARIO=error npm run mock`。
- 接続エラーのとき、伝送設定で新しいパスフレーズを適用すると、約 2 秒後に接続が戻ります。

`npm run build` で `web/dist` に静的ファイルができます。機器ではデーモンがこれを配ります（ネット接続は不要です）。

## 中身

| 場所 | 内容 |
| --- | --- |
| `web/src` | 画面（React + TypeScript） |
| `web/mock` | API のモック |
| `api/openapi.yaml` | API の仕様 |
| `docs/screens.md` | 画面の動きと、決まっていないこと |
| `docs/api.md` | API の決まりと WebSocket |
| `docs/architecture.md` | 機器側の構成（隠し方、オフライン、ライセンス） |
| `docs/terms.md` | 用語の言い換え対応表 |
| `design/screens` | デザインの画像 |
| `CLAUDE.md` | Claude Code 向けの作業ルール |
