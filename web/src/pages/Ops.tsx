import { useEffect, useRef, useState } from 'react';
import { api } from '../api/client';
import { useLive } from '../api/live';
import type { DeviceEvent, Metric, Role, Stats, StatsHistory, Status } from '../api/types';
import { AlertDialog } from '../components/Dialog';
import { Chevron, FullscreenIcon, Glyph, toneText, type Tone } from '../components/Glyph';
import { mediaMeta, PreviewImage } from '../components/Preview';
import { Shell } from '../components/Shell';
import { Sparkline } from '../components/Sparkline';
import { eventText } from '../i18n/ja';
import { fixed1, int, timeOf } from '../lib/format';
import { openFullscreen, takeReturnFocus } from '../lib/fullscreen';
import { href } from '../lib/router';

interface Readout {
  label: string;
  value: string;
  unit: string;
  ref: string;
  tone: Tone | null;
  spark: { metric: Metric; lo: number; hi: number; threshold?: number; ticks?: boolean };
}

const NA = '—';

function readouts(role: Role, st: Stats | null, status: Status): Readout[] {
  const stopped = status.state === 'stopped';
  const live = status.state === 'live' && st !== null;
  const off = (label: string, _unit: string, ref: string, spark: Readout['spark']): Readout => ({
    label,
    value: NA,
    unit: '',
    ref: stopped ? '停止中' : ref,
    tone: null,
    spark,
  });
  const loss = (): Readout => {
    const sp = { metric: 'loss_pct' as const, lo: 0, hi: 3, threshold: st?.loss_threshold_pct ?? 1 };
    if (!live || st.loss_pct === null) return off('パケット損失', '%', '未取得', sp);
    const high = st.loss_pct > st.loss_threshold_pct;
    return {
      label: 'パケット損失',
      value: fixed1(st.loss_pct),
      unit: '%',
      ref: `基準 ${fixed1(st.loss_threshold_pct)}% 以下`,
      tone: high ? 'warn' : null,
      spark: sp,
    };
  };

  if (role === 'tx') {
    const br = { metric: 'send_mbps' as const, lo: 10, hi: 14, threshold: st?.target_mbps ?? undefined };
    const rtt = { metric: 'rtt_ms' as const, lo: 20, hi: 60 };
    const bw = { metric: 'bandwidth_mbps' as const, lo: 0, hi: 60 };
    const last = st?.last_rtt_ms != null ? `未取得（前回 ${int(st.last_rtt_ms)} ms）` : '未取得';
    return [
      live && st.send_mbps !== null
        ? {
            label: '送信ビットレート',
            value: fixed1(st.send_mbps),
            unit: 'Mbps',
            ref: st.target_mbps !== null ? `設定 ${fixed1(st.target_mbps)} Mbps（再送分を含む）` : '',
            tone: null,
            spark: br,
          }
        : off('送信ビットレート', 'Mbps', '未接続のため取得できません', br),
      loss(),
      live && st.rtt_ms !== null
        ? { label: 'RTT', value: int(st.rtt_ms), unit: 'ms', ref: '往復の遅れ', tone: null, spark: rtt }
        : off('RTT', 'ms', last, rtt),
      live && st.bandwidth_mbps !== null
        ? { label: '推定帯域', value: int(st.bandwidth_mbps), unit: 'Mbps', ref: '回線から推定', tone: null, spark: bw }
        : off('推定帯域', 'Mbps', '未取得', bw),
    ];
  }

  const br = { metric: 'recv_mbps' as const, lo: 10, hi: 14 };
  const drop = { metric: 'frames_dropped' as const, lo: 0, hi: 1, ticks: true };
  const lat = { metric: 'video_latency_ms' as const, lo: 280, hi: 340 };
  const bufRef = `伝送バッファは ${st?.buffer_ms ?? '—'} ms`;
  return [
    live && st.recv_mbps !== null
      ? { label: '受信ビットレート', value: fixed1(st.recv_mbps), unit: 'Mbps', ref: '直近 1 秒の平均', tone: null, spark: br }
      : off('受信ビットレート', 'Mbps', '未接続のため取得できません', br),
    loss(),
    live && st.frames_dropped_5m !== null
      ? {
          label: '映像の欠落',
          value: String(st.frames_dropped_5m),
          unit: 'フレーム',
          ref: '直近 5 分',
          tone: st.frames_dropped_5m > 0 ? 'warn' : null,
          spark: drop,
        }
      : off('映像の欠落', 'フレーム', '未取得', drop),
    live && st.video_latency_ms !== null
      ? { label: '映像遅延（実測）', value: int(st.video_latency_ms), unit: 'ms', ref: bufRef, tone: null, spark: lat }
      : { ...off('映像遅延（実測）', 'ms', bufRef, lat), ref: bufRef },
  ];
}

function details(role: Role, st: Stats | null, status: Status): { label: string; value: string; warn?: boolean }[] {
  const stopped = status.state === 'stopped';
  const live = status.state === 'live' && st !== null;
  const na = stopped ? '— 停止中' : '— 未取得';
  if (role === 'tx') {
    return [
      { label: '再送率', value: live && st.retrans_pct !== null ? `${fixed1(st.retrans_pct)} %` : na },
      {
        label: '送信が間に合わなかったパケット（5 分）',
        value: live && st.send_late_5m !== null ? String(st.send_late_5m) : na,
        warn: live && (st.send_late_5m ?? 0) > 0,
      },
      {
        label: `伝送バッファ（${st?.buffer_mode === 'manual' ? '手動' : '自動'}）`,
        value: st ? `${st.buffer_ms} ms` : NA,
      },
    ];
  }
  const last = st?.last_rtt_ms != null && !stopped ? `— 未取得（前回 ${int(st.last_rtt_ms)} ms）` : na;
  return [
    { label: 'RTT', value: live && st.rtt_ms !== null ? `${int(st.rtt_ms)} ms` : last },
    { label: '再送の割合', value: live && st.retrans_pct !== null ? `${fixed1(st.retrans_pct)} %` : na },
    {
      label: '回復できなかったパケット（5 分）',
      value: live && st.unrecovered_5m !== null ? String(st.unrecovered_5m) : na,
      warn: live && (st.unrecovered_5m ?? 0) > 0,
    },
  ];
}

interface Stage {
  name: string;
  tone: Tone | null;
  note: string;
}

function signalPath(role: Role, status: Status, st: Stats | null): { stages: Stage[]; dashedFrom: (i: number) => boolean } {
  const peer = status.peer.id ?? '';
  const has = (c: string) => status.alerts.some((a) => a.code === c);
  const loss = status.alerts.find((a) => a.code === 'loss_high');
  const lossNote = loss ? `損失 ${fixed1(Number(loss.params?.loss_pct ?? st?.loss_pct ?? 0))}%` : '';
  const s = (name: string, tone: Tone | null = null, note = ''): Stage => ({ name, tone, note });

  if (role === 'tx') {
    const names = ['入力', 'エンコード', '送出', '回線', `受信機 ${peer}`];
    if (status.state === 'stopped')
      return {
        stages: [s(names[0]), s(names[1]), s(names[2], 'off', '停止中'), s(names[3]), s(names[4], 'off', '未接続')],
        dashedFrom: (i) => i >= 2,
      };
    if (status.state === 'reconnecting' || status.state === 'connecting') {
      const peerNote = has('peer_rejected_passphrase') ? '接続を拒否' : has('peer_unreachable') ? '応答なし' : '未接続';
      return {
        stages: [
          s(names[0]),
          s(names[1]),
          s(names[2], 'warn', status.state === 'reconnecting' ? '再接続中' : '接続中'),
          s(names[3], 'na', '未取得'),
          s(names[4], peerNote === '未接続' ? 'off' : 'err', peerNote),
        ],
        dashedFrom: (i) => i >= 2,
      };
    }
    return {
      stages: [s(names[0]), s(names[1]), s(names[2]), loss ? s(names[3], 'warn', lossNote) : s(names[3]), s(names[4])],
      dashedFrom: () => false,
    };
  }

  const names = [`送信機 ${peer}`, '回線', '受信', 'デコード', '映像出力'];
  if (status.state === 'stopped')
    return {
      stages: [s(names[0], 'off', '未接続'), s(names[1]), s(names[2]), s(names[3]), s(names[4], 'off', '黒を出力')],
      dashedFrom: (i) => i <= 2,
    };
  if (status.state !== 'live') {
    const rejected = has('peer_rejected_passphrase');
    return {
      stages: [
        s(names[0], 'off', '未接続'),
        s(names[1], 'na', '未取得'),
        rejected ? s(names[2], 'err', '拒否') : s(names[2], 'off', '待ち受け中'),
        s(names[3], 'na', '未取得'),
        s(names[4], 'off', '黒を出力'),
      ],
      dashedFrom: (i) => i <= 2,
    };
  }
  const frames = st?.frames_dropped_5m ?? 0;
  return {
    stages: [
      s(names[0]),
      loss ? s(names[1], 'warn', lossNote) : s(names[1]),
      s(names[2]),
      frames > 0 ? s(names[3], 'warn', `欠落 ${frames} フレーム`) : s(names[3]),
      s(names[4]),
    ],
    dashedFrom: () => false,
  };
}

const eventTone = (e: DeviceEvent): Tone | null => (e.level === 'error' ? 'err' : e.level === 'warn' ? 'warn' : null);

const seriesOf = (h: StatsHistory | null, m: Metric) => h?.series[m] ?? [];

export function OpsPage() {
  const { device, status, stats, history, events, setStatus } = useLive();
  const [more, setMore] = useState(false);
  const [askStop, setAskStop] = useState(false);
  const [busy, setBusy] = useState(false);
  const fsBtn = useRef<HTMLButtonElement>(null);

  // Return focus to the 全画面 button after leaving the full-screen view.
  useEffect(() => {
    if (takeReturnFocus()) fsBtn.current?.focus();
  }, []);

  // F opens the full-screen view (not while typing).
  useEffect(() => {
    const on = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement;
      if (e.key.toLowerCase() !== 'f' || e.metaKey || e.ctrlKey || e.altKey) return;
      if (t.closest('input, select, textarea, [role="alertdialog"]')) return;
      e.preventDefault();
      openFullscreen();
    };
    addEventListener('keydown', on);
    return () => removeEventListener('keydown', on);
  }, []);

  if (!device || !status) {
    return (
      <Shell route="ops">
        <p className="muted">読み込み中…</p>
      </Shell>
    );
  }
  const role = device.role;
  const tx = role === 'tx';
  const stopped = status.state === 'stopped';
  const reads = readouts(role, stats, status);
  const rows = details(role, stats, status);
  const path = signalPath(role, status, stats);

  const run = async (f: () => Promise<Status>) => {
    setBusy(true);
    try {
      setStatus(await f());
    } finally {
      setBusy(false);
    }
  };

  return (
    <Shell route="ops">
      <h1 className="sr-only">運用</h1>
      <div className="card ops-card">
        <div className="ops-top">
          <section aria-labelledby="pv" className="ops-media">
            <div className="pv-head">
              <h2 id="pv">{tx ? '入力映像' : '受信映像'}</h2>
              <div className="pv-meta">
                <span>{mediaMeta(status.media)}</span>
                <button ref={fsBtn} type="button" className="btn btn-sm" title="全画面で表示（F キー）" onClick={openFullscreen}>
                  <FullscreenIcon />
                  全画面
                </button>
              </div>
            </div>
            <PreviewImage media={status.media} role={role} />
            <div className="audio-line">
              <span>音声</span>
              <span>なし</span>
            </div>
            <section aria-labelledby="path" className="path">
              <h2 id="path">信号経路</h2>
              <ol>
                {path.stages.map((p, i) => (
                  <li key={p.name}>
                    <span
                      className="stage"
                      style={{
                        color: p.tone ? toneText[p.tone] : stopped && i > 0 ? 'var(--sub)' : 'var(--ink)',
                        fontWeight: p.tone === 'warn' || p.tone === 'err' ? 600 : 400,
                      }}
                    >
                      {p.tone && <Glyph tone={p.tone} />}
                      {p.name}
                      {p.note && <span style={{ fontWeight: 400 }}>{p.note}</span>}
                    </span>
                    {i < path.stages.length - 1 && (
                      <span aria-hidden="true" className={path.dashedFrom(i) ? 'conn dashed' : 'conn'} />
                    )}
                  </li>
                ))}
              </ol>
            </section>
          </section>

          <section aria-labelledby="st" className="ops-side">
            <h2 id="st" className="sr-only">
              {tx ? '伝送の状態' : '受信の状態'}
            </h2>
            {reads.map((r) => (
              <div key={r.label}>
                <div className="readout-label">
                  {r.label}
                  {r.tone && <Glyph tone={r.tone} />}
                </div>
                <div className="readout-row">
                  <div className="readout-value">
                    <b style={{ color: r.tone === 'warn' ? 'var(--warn-text)' : r.value === NA ? 'var(--sub)' : 'var(--ink)' }}>
                      {r.value}
                    </b>
                    <span>{r.unit}</span>
                  </div>
                  <Sparkline
                    values={seriesOf(history, r.spark.metric)}
                    lo={r.spark.lo}
                    hi={r.spark.hi}
                    threshold={r.spark.threshold}
                    ticks={r.spark.ticks}
                    label={`${r.label}の推移（直近 5 分）`}
                  />
                </div>
                <div className="readout-ref">{r.ref}</div>
              </div>
            ))}
            <p className="spark-note">線は直近 5 分の推移（破線は基準）</p>
            <div>
              <button type="button" className="disclosure" aria-expanded={more} onClick={() => setMore(!more)}>
                <Chevron />
                回線の詳細
              </button>
              {more && (
                <dl className="details">
                  {rows.map((d) => (
                    <div key={d.label}>
                      <dt>{d.label}</dt>
                      <dd style={{ color: d.warn ? 'var(--warn-text)' : d.value.startsWith('—') ? 'var(--sub)' : 'var(--ink)' }}>
                        {d.value}
                      </dd>
                    </div>
                  ))}
                </dl>
              )}
            </div>
            {stopped ? (
              <button type="button" className="btn btn-primary" disabled={busy} onClick={() => run(api.start)}>
                {tx ? '伝送を開始' : '受信を開始'}
              </button>
            ) : (
              <button type="button" className="btn" onClick={() => setAskStop(true)}>
                {tx ? '伝送を停止…' : '受信を停止…'}
              </button>
            )}
          </section>
        </div>

        <section aria-labelledby="ev" className="events">
          <div className="events-head">
            <h2 id="ev">イベント</h2>
            <a href={href('logs')}>ログをすべて表示</a>
          </div>
          <ul>
            {events.slice(0, 4).map((e) => {
              const tone = eventTone(e);
              return (
                <li key={e.id}>
                  <time dateTime={e.at}>{timeOf(e.at)}</time>
                  <span>{tone && <Glyph tone={tone} />}</span>
                  <span>{eventText(role, e, status.peer.id)}</span>
                </li>
              );
            })}
          </ul>
        </section>
      </div>

      {askStop && (
        <AlertDialog
          title={tx ? '伝送を停止しますか' : '受信を停止しますか'}
          description={
            tx
              ? `受信機 ${status.peer.id ?? ''} への送信を止めます。受信側の映像も止まります。`
              : `送信機 ${status.peer.id ?? ''} との接続を切ります。映像出力は黒になります。`
          }
          onCancel={() => setAskStop(false)}
          actions={
            <>
              <button type="button" className="btn" onClick={() => setAskStop(false)} data-autofocus>
                キャンセル
              </button>
              <button
                type="button"
                className="btn btn-danger"
                disabled={busy}
                onClick={async () => {
                  await run(api.stop);
                  setAskStop(false);
                }}
              >
                {tx ? '伝送を停止する' : '受信を停止する'}
              </button>
            </>
          }
        />
      )}
    </Shell>
  );
}
