import { useState } from 'react';
import { useLive } from '../api/live';

/**
 * Development only (not in production builds): switches the mock server between
 * transmitter/receiver and between scenarios. The device daemon has no /mock API.
 */
export function DevBar() {
  const { device, refresh } = useLive();
  const [scenario, setScenario] = useState(() => new URLSearchParams(location.search).get('scenario') ?? '');
  const post = async (path: string, body: unknown) => {
    await fetch(path, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
    await refresh();
  };
  return (
    <div className="devbar" aria-label="モックの切り替え（開発用）">
      <span>モック</span>
      <label>
        機器{' '}
        <select value={device?.role ?? 'tx'} onChange={(e) => post('/mock/role', { role: e.target.value })}>
          <option value="tx">送信機</option>
          <option value="rx">受信機</option>
        </select>
      </label>
      <label>
        状況{' '}
        <select
          value={scenario}
          onChange={(e) => {
            setScenario(e.target.value);
            post('/mock/scenario', { scenario: e.target.value });
          }}
        >
          <option value="" disabled>
            選ぶ
          </option>
          <option value="normal">正常</option>
          <option value="warn">回線警告</option>
          <option value="error">接続エラー</option>
        </select>
      </label>
    </div>
  );
}
