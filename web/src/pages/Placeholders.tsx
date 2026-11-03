/**
 * Screens that have not been designed yet. They are kept minimal and clearly
 * marked so the menu works end to end; replace them once designs exist
 * (docs/screens.md「未デザインの画面」).
 */
import { useEffect, useRef, useState, type ReactNode } from 'react';
import { api } from '../api/client';
import { useLive } from '../api/live';
import type { DeviceEvent, NetworkTest } from '../api/types';
import { Glyph } from '../components/Glyph';
import { Shell } from '../components/Shell';
import { eventText, roleName } from '../i18n/ja';
import { fixed1, timeOf } from '../lib/format';
import type { Route } from '../lib/router';

function Todo({ note }: { note: ReactNode }) {
  return (
    <div className="todo-note">
      <span style={{ marginTop: 4 }}>
        <Glyph tone="info" />
      </span>
      <span>{note}</span>
    </div>
  );
}

export function NotDesigned({ route, title, note }: { route: Route; title: string; note: string }) {
  return (
    <Shell route={route}>
      <div className="card todo">
        <h1>{title}</h1>
        <Todo note={`${note}この画面は仮です。`} />
      </div>
    </Shell>
  );
}

export const RecordingPage = () => (
  <NotDesigned route="recording" title="録画" note="録画の画面はまだデザインしていません。" />
);

export function LogsPage() {
  const { device, status } = useLive();
  const [rows, setRows] = useState<DeviceEvent[]>([]);
  useEffect(() => {
    api.events(200).then(setRows).catch(() => undefined);
  }, []);
  return (
    <Shell route="logs">
      <div className="card todo">
        <h1>ログ</h1>
        <Todo note="一覧の形は仮です。絞り込みや書き出しは、この画面のデザインで決めます。" />
        <table className="table" style={{ marginTop: 16 }}>
          <thead>
            <tr>
              <th style={{ width: 96 }}>時刻</th>
              <th style={{ width: 72 }}>種類</th>
              <th>内容</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((e) => (
              <tr key={e.id}>
                <td className="muted">{timeOf(e.at)}</td>
                <td>
                  <span style={{ display: 'inline-flex', alignItems: 'center', gap: 6 }}>
                    {e.level === 'info' ? (
                      <span className="muted">情報</span>
                    ) : (
                      <>
                        <Glyph tone={e.level === 'error' ? 'err' : 'warn'} />
                        {e.level === 'error' ? 'エラー' : '警告'}
                      </>
                    )}
                  </span>
                </td>
                <td>{device ? eventText(device.role, e, status?.peer.id ?? null) : ''}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </Shell>
  );
}

export function NetworkPage() {
  const { device, status } = useLive();
  const [test, setTest] = useState<NetworkTest | null>(null);
  const [error, setError] = useState<string | null>(null);
  const timer = useRef<ReturnType<typeof setInterval> | undefined>(undefined);
  useEffect(() => () => clearInterval(timer.current), []);

  const start = async () => {
    setError(null);
    try {
      const t = await api.networkTest.start({ direction: 'to_peer', duration_s: 10 });
      setTest(t);
      clearInterval(timer.current);
      timer.current = setInterval(async () => {
        const cur = await api.networkTest.get(t.id);
        setTest(cur);
        if (cur.state !== 'running') clearInterval(timer.current);
      }, 500);
    } catch {
      setError('回線テストを始められませんでした。伝送を止めてからもう一度お試しください。');
    }
  };

  const tx = device?.role === 'tx';
  return (
    <Shell route="network">
      <div className="card todo">
        <h1>ネットワーク</h1>
        <Todo note="この画面はまだデザインしていません。回線テストの動作を確かめるための仮の画面です。" />
        {tx ? (
          <section aria-labelledby="nt" style={{ marginTop: 20 }}>
            <h2 id="nt" style={{ margin: 0, fontSize: 16, fontWeight: 600 }}>
              回線テスト
            </h2>
            <p className="muted" style={{ margin: '2px 0 12px', fontSize: 13 }}>
              受信機 {status?.peer.id ?? ''} までの帯域を 10 秒間はかります。伝送中は映像に影響するため、止めてから行ってください。
            </p>
            <button type="button" className="btn btn-primary" onClick={start} disabled={test?.state === 'running'}>
              {test?.state === 'running' ? 'はかっています…' : '回線テストを始める'}
            </button>
            {error && <p style={{ color: 'var(--err-text)', fontWeight: 600 }}>{error}</p>}
            {test && (
              <div style={{ marginTop: 16, maxWidth: 480 }}>
                <div className="progress" aria-label="進み具合">
                  <div style={{ width: `${Math.round(test.progress * 100)}%` }} />
                </div>
                {test.result && (
                  <dl className="kv" style={{ marginTop: 12 }}>
                    <dt>帯域</dt>
                    <dd>{fixed1(test.result.throughput_mbps)} Mbps</dd>
                    <dt>パケット損失</dt>
                    <dd>{fixed1(test.result.loss_pct)} %</dd>
                    <dt>揺らぎ</dt>
                    <dd>{fixed1(test.result.jitter_ms)} ms</dd>
                  </dl>
                )}
                {test.state === 'failed' && <p style={{ color: 'var(--err-text)', fontWeight: 600 }}>受信機に接続できませんでした。</p>}
              </div>
            )}
          </section>
        ) : (
          <p className="muted" style={{ marginTop: 16 }}>
            回線テストは送信機から始めます。受信機側は、テストの間だけ自動で応答します。
          </p>
        )}
      </div>
    </Shell>
  );
}

export function DevicePage() {
  const { device } = useLive();
  return (
    <Shell route="device">
      <div className="card todo">
        <h1>機器情報</h1>
        <Todo note="この画面はまだデザインしていません。項目は仮です。" />
        {device && (
          <dl className="kv" style={{ marginTop: 16 }}>
            <dt>機器名</dt>
            <dd>
              {device.id}（{roleName(device.role)}）
            </dd>
            <dt>IP アドレス</dt>
            <dd>{device.address}</dd>
            <dt>ソフトウェア</dt>
            <dd>{device.firmware}</dd>
            <dt>製造番号</dt>
            <dd>{device.serial}</dd>
            <dt>時刻</dt>
            <dd>日本時間（JST）</dd>
          </dl>
        )}
        <h2 style={{ margin: '28px 0 4px', fontSize: 16, fontWeight: 600 }}>ライセンス</h2>
        <p className="muted" style={{ margin: 0, fontSize: 13 }}>
          この機器が使っているオープンソースソフトウェアの一覧と、ライセンス本文を表示します（内容は出荷前に確定します）。
        </p>
      </div>
    </Shell>
  );
}
