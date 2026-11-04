import { useEffect, useRef, useState } from 'react';
import { ApiFailure, api } from '../api/client';
import { useLive } from '../api/live';
import type { DeviceEvent, Metric, Role, Stats, StatsHistory, Status } from '../api/types';
import { AlertDialog } from '../components/Dialog';
import { Chevron, FullscreenIcon, Glyph, toneText, type Tone } from '../components/Glyph';
import { mediaMeta, OpsPreview } from '../components/Preview';
import { Shell } from '../components/Shell';
import { Sparkline } from '../components/Sparkline';
import { delayName, eventText, startProblem } from '../i18n/ja';
import { fixed1, int, timeOf } from '../lib/format';
import { openFullscreen, takeReturnFocus } from '../lib/fullscreen';
import { href } from '../lib/router';

interface Spark {
  metric: Metric;
  minSpan: number;
  floor?: number;
  /** dashed series behind the line */
  reference?: Metric;
  /** dashed horizontal line */
  threshold?: number;
  ticks?: boolean;
}

interface Readout {
  label: string;
  value: string;
  unit: string;
  ref: string;
  tone: Tone | null;
  spark?: Spark;
}

const NA = '—';

const hasAlert = (status: Status, code: string) => status.alerts.some((a) => a.code === code);

function readouts(role: Role, st: Stats | null, status: Status): Readout[] {
  const stopped = status.state === 'stopped';
  const live = status.state === 'live' && st !== null;
  const off = (label: string, ref: string, spark?: Spark): Readout => ({
    label,
    value: NA,
    unit: '',
    ref: stopped ? '停止中' : ref,
    tone: null,
    spark,
  });
  const noLink = '未接続のため取得できません';
  const loss = (): Readout => {
    const thr = st?.loss_threshold_pct ?? null;
    const sp: Spark = { metric: 'loss_pct', minSpan: 2, floor: 0, ...(thr !== null ? { threshold: thr } : {}) };
    if (!live || st.loss_pct === null) return off('回線の損失', live ? '未取得' : noLink, sp);
    // In the standard profile losses are normal and recovered by resending: no threshold, no warning.
    return {
      label: '回線の損失',
      value: fixed1(st.loss_pct),
      unit: '%',
      ref: thr === null ? '再送で補っています' : `基準 ${fixed1(thr)}% 以下`,
      tone: thr !== null && st.loss_pct > thr ? 'warn' : null,
      spark: sp,
    };
  };

  if (role === 'tx') {
    const quality: Spark = { metric: 'actual_mbps', reference: 'target_mbps', minSpan: 1, floor: 0 };
    // The device says below which margin it warns (delay_margin_low); drawn as the dashed line.
    const warnS = st?.delay_margin_warn_s ?? null;
    const margin: Spark = { metric: 'delay_margin_s', minSpan: 4, floor: 0, ...(warnS !== null ? { threshold: warnS } : {}) };
    const send: Spark = { metric: 'send_mbps', minSpan: 1, floor: 0 };
    const target = live && st.target_mbps !== null ? `目標 ${fixed1(st.target_mbps)} Mbps` : '';
    const low =
      live && (hasAlert(status, 'delay_margin_low') || (warnS !== null && st.delay_margin_s !== null && st.delay_margin_s < warnS));
    const marginRef = low
      ? warnS !== null
        ? `${warnS} 秒を下回っています`
        : '余裕が少なくなっています'
      : warnS !== null
        ? `破線の ${warnS} 秒を下回ると警告します`
        : '';
    return [
      live && st.actual_mbps !== null
        ? { label: '画質（実際）', value: fixed1(st.actual_mbps), unit: 'Mbps', ref: target, tone: null, spark: quality }
        : off('画質（実際）', live ? target || '未取得' : noLink, quality),
      live && st.delay_margin_s !== null
        ? {
            label: '映像が止まるまでの余裕',
            value: fixed1(st.delay_margin_s),
            unit: '秒',
            ref: marginRef,
            tone: low ? 'warn' : null,
            spark: margin,
          }
        : off('映像が止まるまでの余裕', live ? '未取得' : noLink, margin),
      loss(),
      live && st.send_mbps !== null
        ? { label: '送っているデータ量', value: fixed1(st.send_mbps), unit: 'Mbps', ref: '直近 6 秒の平均。送り直した分を含む', tone: null, spark: send }
        : off('送っているデータ量', live ? '未取得' : noLink, send),
    ];
  }

  const recv: Spark = { metric: 'recv_mbps', minSpan: 1, floor: 0 };
  const stall: Spark = { metric: 'stall_s', minSpan: 1, ticks: true };
  const measured = live && st.video_latency_ms !== null;
  const target = st ? fixed1(st.delay_target_ms / 1000) : NA;
  return [
    live && st.recv_mbps !== null
      ? { label: '受け取っているデータ量', value: fixed1(st.recv_mbps), unit: 'Mbps', ref: '直近 6 秒の平均', tone: null, spark: recv }
      : off('受け取っているデータ量', live ? '未取得' : noLink, recv),
    loss(),
    live && st.stalls_5m !== null
      ? {
          label: '直近 5 分で映像が止まった回数',
          value: String(st.stalls_5m),
          unit: '回',
          ref: st.stalls_5m > 0 ? `合計 ${fixed1(st.stall_s_5m ?? 0)} 秒` : '止まっていません',
          tone: st.stalls_5m > 0 ? 'warn' : null,
          spark: stall,
        }
      : off('直近 5 分で映像が止まった回数', live ? '未取得' : noLink, stall),
    measured
      ? { label: '映像の遅れ', value: fixed1(st.video_latency_ms! / 1000), unit: '秒', ref: `入力から表示まで（実測）。目標 ${target} 秒`, tone: null }
      : live
        ? { label: '映像の遅れ（目安）', value: target, unit: '秒', ref: '入力から表示まで。まだ実測していません', tone: null }
        : off('映像の遅れ（目安）', noLink),
  ];
}

function details(role: Role, st: Stats | null, status: Status): { label: string; value: string; warn?: boolean }[] {
  const stopped = status.state === 'stopped';
  const live = status.state === 'live' && st !== null;
  const na = stopped ? '— 停止中' : '— 未取得';
  const rtt = {
    label: '回線の往復時間',
    value: live && st.rtt_ms !== null ? `${int(st.rtt_ms)} ms` : st?.last_rtt_ms != null && !stopped ? `— 未取得（前回 ${int(st.last_rtt_ms)} ms）` : na,
  };
  const delay = { label: '遅延の設定', value: st ? delayName(st.delay_profile, st.delay_target_ms) : NA };
  if (role === 'tx') {
    return [
      rtt,
      {
        label: '回線の推定容量',
        value: live
          ? st.bandwidth_mbps !== null
            ? `${fixed1(st.bandwidth_mbps)} Mbps`
            : '— 余裕あり（混んだときだけ測ります）'
          : na,
      },
      {
        label: '送りきれずに捨てたデータ（直近 5 分）',
        value: live && st.send_late_5m !== null ? String(st.send_late_5m) : na,
        warn: live && (st.send_late_5m ?? 0) > 0,
      },
      delay,
    ];
  }
  return [
    rtt,
    { label: '送り直して届いた割合', value: live && st.retrans_pct !== null ? `${fixed1(st.retrans_pct)} %` : na },
    {
      label: '届かなかったデータ（直近 5 分）',
      value: live && st.unrecovered_5m !== null ? String(st.unrecovered_5m) : na,
      warn: live && (st.unrecovered_5m ?? 0) > 0,
    },
    delay,
  ];
}

interface Stage {
  name: string;
  tone: Tone | null;
  note: string;
}

function signalPath(role: Role, status: Status): { stages: Stage[]; dashedFrom: (i: number) => boolean } {
  const peer = status.peer.id ?? '';
  const has = (c: string) => hasAlert(status, c);
  const find = (c: string) => status.alerts.find((a) => a.code === c);
  const loss = find('loss_high');
  const lossNote = loss ? `損失 ${fixed1(Number(loss.params?.loss_pct ?? 0))}%` : '';
  const s = (name: string, tone: Tone | null = null, note = ''): Stage => ({ name, tone, note });

  if (role === 'tx') {
    const names = ['入力', '圧縮', '送出', '回線', `受信機 ${peer}`];
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
    const crowded = has('delay_margin_low');
    return {
      stages: [
        s(names[0]),
        s(names[1]),
        s(names[2]),
        crowded ? s(names[3], 'warn', '混み合っています') : loss ? s(names[3], 'warn', lossNote) : s(names[3]),
        s(names[4]),
      ],
      dashedFrom: () => false,
    };
  }

  const names = [`送信機 ${peer}`, '回線', '受信', '復元', '映像出力'];
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
  const stalled = find('video_stalled');
  return {
    stages: [
      s(names[0]),
      loss ? s(names[1], 'warn', lossNote) : s(names[1]),
      s(names[2]),
      s(names[3]),
      stalled ? s(names[4], 'warn', `${stalled.params?.count ?? ''} 回停止`) : s(names[4]),
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
  const [startNote, setStartNote] = useState<string | null>(null);
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
  const path = signalPath(role, status);

  const run = async (f: () => Promise<Status>) => {
    setBusy(true);
    try {
      setStatus(await f());
    } finally {
      setBusy(false);
    }
  };
  // Start can be refused, e.g. while the line is being measured (409 busy).
  const start = async () => {
    setStartNote(null);
    try {
      await run(api.start);
    } catch (e) {
      setStartNote(startProblem(role, e instanceof ApiFailure ? e.body?.error.code : undefined));
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
            <OpsPreview status={status} role={role} />
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
                  {r.spark && (
                    <Sparkline
                      values={seriesOf(history, r.spark.metric)}
                      reference={r.spark.reference ? seriesOf(history, r.spark.reference) : undefined}
                      threshold={r.spark.threshold}
                      minSpan={r.spark.minSpan}
                      floor={r.spark.floor}
                      ticks={r.spark.ticks}
                      label={`${r.label}の推移（直近 5 分）`}
                    />
                  )}
                </div>
                <div className="readout-ref">{r.ref}</div>
              </div>
            ))}
            <p className="spark-note">
              {tx ? '線は直近 5 分の推移（破線は目標と警告の線）' : '線は直近 5 分の推移（縦線は映像の停止）'}
            </p>
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
                        {d.warn ? (
                          // A warning is never shown by colour alone: shape and words too.
                          <span style={{ display: 'inline-flex', alignItems: 'center', gap: 6 }}>
                            <Glyph tone="warn" />
                            {d.value}（映像が乱れたおそれ）
                          </span>
                        ) : (
                          d.value
                        )}
                      </dd>
                    </div>
                  ))}
                </dl>
              )}
            </div>
            {stopped ? (
              <>
                <button type="button" className="btn btn-primary" disabled={busy} onClick={start}>
                  {tx ? '伝送を開始' : '受信を開始'}
                </button>
                {startNote && (
                  <p role="alert" style={{ display: 'flex', alignItems: 'flex-start', gap: 8, margin: 0, fontWeight: 600, color: 'var(--err-text)' }}>
                    <span style={{ marginTop: 1 }}>
                      <Glyph tone="err" />
                    </span>
                    <span>{startNote}</span>
                  </p>
                )}
              </>
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
