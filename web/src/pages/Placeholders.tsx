/**
 * Screens that have not been designed yet. They are kept minimal and clearly
 * marked so the menu works end to end; replace them once designs exist
 * (docs/screens.md「未デザインの画面」).
 */
import { useEffect, useState, type ReactNode } from 'react';
import { api } from '../api/client';
import { useLive } from '../api/live';
import type { DeviceEvent } from '../api/types';
import { Glyph } from '../components/Glyph';
import { Shell } from '../components/Shell';
import { eventText, roleName } from '../i18n/ja';
import { timeOf } from '../lib/format';
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
