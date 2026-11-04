/**
 * ネットワーク: checks of the line to the peer, started from the browser.
 *   つながりの確認  whether the peer and the devices on the way answer (both roles, also while live)
 *   回線の速さ      how fast data gets through (transmitter only, while the link is stopped)
 *   これまでの結果  the device keeps the last 20 tests in memory
 * The design canvas has no page for this yet; it is built from the existing parts
 * (docs/screens.md「ネットワーク」).
 */
import { Fragment, useCallback, useEffect, useRef, useState } from 'react';
import { ApiFailure, api } from '../api/client';
import { useLive } from '../api/live';
import type {
  NetworkTest,
  NetworkTestDirection,
  NetworkTestMode,
  NetworkTestOptions,
  ReachabilityResult,
  ReachabilityTarget,
  Role,
  ThroughputResult,
} from '../api/types';
import { Glyph, toneText, type Tone } from '../components/Glyph';
import { Segmented } from '../components/Segmented';
import { Row } from '../components/Settings';
import { Shell } from '../components/Shell';
import {
  directionChoice,
  directionText,
  fieldErrorText,
  reachFailed,
  reachLabel,
  reachLevel,
  reachNoPeer,
  targetName,
  testModeName,
  testModeShort,
  testStartProblem,
  throughputBlocked,
  throughputErrorShort,
  throughputErrorText,
  videoVerdict,
  type ReachLevel,
} from '../i18n/ja';
import { fixed1, timeOf } from '../lib/format';
import { href } from '../lib/router';

type Reach = Extract<NetworkTest, { kind: 'reachability' }>;
type Tput = Extract<NetworkTest, { kind: 'throughput' }>;

/** Fallbacks until the device's options arrive. */
const BITRATE_RANGE: [number, number] = [1.5, 4.0];
const STEADY_RATE = 6;
const RATE_CHOICES = [2, 4, 6, 8, 10];
const DURATION_CHOICES = [10, 20, 30];
const DEFAULT_DURATION = 10;

const LEVEL_TONE: Record<ReachLevel, Tone> = { ok: 'ok', partial: 'warn', none: 'err' };
const VERDICT_TONE = { ok: 'ok', lower: 'warn', short: 'err' } as const;

/** Round-trip time: one decimal below 10 ms (a device next door answers in 0.4 ms). */
const ms = (v: number) => (v < 10 ? v.toFixed(1) : String(Math.round(v)));

/** The request as the device ran it, with the defaults where a field is missing. */
const paramsOf = (t: Tput) => ({
  direction: (t.request?.direction ?? 'to_peer') as NetworkTestDirection,
  mode: (t.request?.mode ?? 'max') as NetworkTestMode,
  rate: t.request?.rate_mbps ?? null,
  duration: t.request?.duration_s ?? DEFAULT_DURATION,
});

const isReach = (t: NetworkTest): t is Reach => t.kind === 'reachability';
const isTput = (t: NetworkTest): t is Tput => t.kind === 'throughput';

export function NetworkPage() {
  const { device, status } = useLive();
  const role = device?.role ?? null;
  const [list, setList] = useState<NetworkTest[] | null>(null);
  const [options, setOptions] = useState<NetworkTestOptions | null>(null);
  const [bitrateRange, setBitrateRange] = useState<[number, number]>(BITRATE_RANGE);
  const [reachNotice, setReachNotice] = useState<string | null>(null);
  const [tputNotice, setTputNotice] = useState<string | null>(null);
  const [posting, setPosting] = useState<'reachability' | 'throughput' | null>(null);
  // Throughput form
  const [direction, setDirection] = useState<NetworkTestDirection>('to_peer');
  const [mode, setMode] = useState<NetworkTestMode>('max');
  const [rate, setRate] = useState<number | null>(null);
  const [duration, setDuration] = useState(DEFAULT_DURATION);

  // A list read that started before a test was posted must not hide the new test.
  const gen = useRef(0);
  const load = useCallback(() => {
    const g = gen.current;
    api.networkTest
      .list()
      .then((r) => g === gen.current && setList(r.items))
      .catch(() => undefined);
  }, []);
  const put = (t: NetworkTest) => {
    gen.current += 1;
    setList((prev) => [t, ...(prev ?? []).filter((x) => x.id !== t.id)].slice(0, 20));
  };

  useEffect(() => {
    if (!role) return;
    load();
    if (role === 'tx') {
      api.networkTest.options().then(setOptions).catch(() => undefined);
      api.video
        .options()
        .then((o) => setBitrateRange(o.bitrate_range_mbps))
        .catch(() => undefined);
    }
  }, [role, load]);

  // Every second while a test runs (progress, the latest second), every 5 s otherwise
  // (a test started from another screen shows up).
  const running = list?.some((t) => t.state === 'running') ?? false;
  useEffect(() => {
    const t = setInterval(load, running ? 1000 : 5000);
    return () => clearInterval(t);
  }, [running, load]);

  if (!device || !status || !role) {
    return (
      <Shell route="network">
        <p className="muted">読み込み中…</p>
      </Shell>
    );
  }

  const tx = role === 'tx';
  const reach = list?.find(isReach) ?? null;
  const tput = list?.find(isTput) ?? null;
  const reachRunning = reach?.state === 'running';
  const tputRunning = tput?.state === 'running';
  const stopped = status.state === 'stopped';

  const [rMin, rMax] = options?.rate_range_mbps ?? [1, 20];
  const defaultRate = options?.steady_rate_mbps ?? STEADY_RATE;
  const rateNow = rate ?? defaultRate;
  const rates = [...new Set([...RATE_CHOICES, defaultRate])].filter((r) => r >= rMin && r <= rMax).sort((a, b) => a - b);
  const [dMin, dMax] = options?.duration_range_s ?? [5, 30];
  const durations = DURATION_CHOICES.filter((d) => d >= dMin && d <= dMax);

  const startReach = async () => {
    setReachNotice(null);
    setPosting('reachability');
    try {
      put(await api.networkTest.start({ kind: 'reachability' }));
    } catch (e) {
      setReachNotice(testStartProblem('reachability', e instanceof ApiFailure ? e.body?.error.code : undefined, false));
    } finally {
      setPosting(null);
    }
  };

  const startTput = async () => {
    setTputNotice(null);
    setPosting('throughput');
    try {
      put(
        await api.networkTest.start({
          kind: 'throughput',
          direction,
          mode,
          duration_s: duration,
          ...(mode === 'steady' ? { rate_mbps: rateNow } : {}),
        }),
      );
    } catch (e) {
      const body = e instanceof ApiFailure ? e.body : null;
      const fieldErrors = body?.error.code === 'invalid' ? (body.error.errors ?? []) : [];
      setTputNotice(
        fieldErrors.length > 0
          ? fieldErrors.map((f) => `${fieldErrorText(f)}。`).join('')
          : testStartProblem('throughput', body?.error.code, status.state === 'live'),
      );
    } finally {
      setPosting(null);
    }
  };

  const peerId = status.peer.id;
  const peerLabel = `${tx ? '受信機' : '送信機'}${peerId ? ` ${peerId} ` : ''}`;

  return (
    <Shell route="network">
      <div className="card">
        <div className="set-head">
          <h1>ネットワーク</h1>
          <p>{peerLabel}までの回線を確かめます。</p>
        </div>

        <section aria-labelledby="sec-reach" className="nt-sec">
          <h2 id="sec-reach" className="sec-title first">
            つながりの確認
          </h2>
          <div className="nt-body">
            <p className="nt-help">
              {tx ? '受信機' : '送信機'}と、途中の無線機まで届くかを確かめます。伝送中でも使えます。
            </p>
            <div className="nt-actions">
              <button
                type="button"
                className="btn btn-primary"
                onClick={startReach}
                disabled={reachRunning || posting === 'reachability'}
              >
                {reachRunning ? '確かめています…' : '確かめる'}
              </button>
              {reach && !reachRunning && (
                <span className="muted">{timeOf(reach.finished_at ?? reach.started_at)} JST に確かめました</span>
              )}
            </div>
            {reachNotice && <ProblemLine text={reachNotice} alert />}
            {reachRunning && <Progress value={reach.progress} label="つながりを確かめている進み具合" />}
            {reach?.state === 'done' && reach.result && <ReachTable role={role} result={reach.result} />}
            {reach?.state === 'failed' && <ProblemLine text={reachFailed} />}
          </div>
        </section>

        <section aria-labelledby="sec-tput" className="nt-sec">
          <h2 id="sec-tput" className="sec-title">
            回線の速さ
          </h2>
          {!tx ? (
            <div className="nt-body">
              <p className="muted" style={{ margin: 0 }}>
                回線の速さは送信機の画面で測ります。
              </p>
            </div>
          ) : (
            <>
              <div className="nt-body">
                <p className="nt-help" style={{ marginBottom: 0 }}>
                  受信機との間で、1 秒あたりどれだけのデータを送れるかを測ります。
                </p>
              </div>
              <Row label="向き" labelId="lbl-dir">
                <Segmented
                  labelledBy="lbl-dir"
                  value={direction}
                  onChange={setDirection}
                  options={(['to_peer', 'from_peer'] as const).map((d) => ({ value: d, label: directionChoice(role, d) }))}
                />
              </Row>
              <Row label="測り方" labelId="lbl-mode" top padLabel>
                <div role="radiogroup" aria-labelledby="lbl-mode" className="choices">
                  {(['max', 'steady'] as const).map((m) => (
                    <label key={m} className={mode === m ? 'choice on' : 'choice'}>
                      <input type="radio" name="nt-mode" checked={mode === m} onChange={() => setMode(m)} />
                      <span>
                        <b>{testModeName(m)}</b>
                        {m === 'max' ? (
                          <span className="nt-choice-hint">回線がどこまで速く送れるか（上限）を測ります</span>
                        ) : (
                          <span className="nt-choice-hint">映像に近い送り方で、届かずに失われた割合と遅れのばらつきを見ます</span>
                        )}
                      </span>
                    </label>
                  ))}
                </div>
                {mode === 'steady' && (
                  <div className="inline" style={{ marginTop: 10 }}>
                    <label htmlFor="nt-rate">送る速さ</label>
                    <select id="nt-rate" className="select nt-select" value={rateNow} onChange={(e) => setRate(Number(e.target.value))}>
                      {rates.map((r) => (
                        <option key={r} value={r}>
                          {r} Mbps
                        </option>
                      ))}
                    </select>
                  </div>
                )}
              </Row>
              <Row label="時間" labelId="lbl-dur">
                <Segmented
                  labelledBy="lbl-dur"
                  value={String(duration)}
                  onChange={(v) => setDuration(Number(v))}
                  options={durations.map((d) => ({ value: String(d), label: `${d} 秒` }))}
                />
              </Row>
              <div className="nt-body nt-run">
                <div className="nt-actions">
                  <button
                    type="button"
                    className="btn btn-primary"
                    onClick={startTput}
                    disabled={!stopped || tputRunning || posting === 'throughput'}
                    aria-describedby={!stopped ? 'nt-blocked' : undefined}
                  >
                    {tputRunning ? '測っています…' : '測る'}
                  </button>
                  {tput && !tputRunning && (
                    <span className="muted">{timeOf(tput.finished_at ?? tput.started_at)} JST に測りました</span>
                  )}
                </div>
                {!stopped && !tputRunning && (
                  <p id="nt-blocked" className="nt-line">
                    <Glyph tone="info" />
                    <span>
                      {throughputBlocked(role, status.state)}{' '}
                      <a href={href('ops')} style={{ fontWeight: 600, whiteSpace: 'nowrap' }}>
                        運用画面を開く
                      </a>
                    </span>
                  </p>
                )}
                {tputNotice && <ProblemLine text={tputNotice} alert />}
                {tput && <TputView role={role} test={tput} bitrateRange={bitrateRange} />}
              </div>
            </>
          )}
        </section>

        <section aria-labelledby="sec-hist" className="nt-sec">
          <h2 id="sec-hist" className="sec-title">
            これまでの結果
          </h2>
          <div className="nt-body">
            <p className="nt-help">新しい順に 20 件まで。機器を再起動すると消えます。</p>
            {list && list.length > 0 ? (
              <div className="nt-table-wrap">
                <table className="table nt-hist">
                  <thead>
                    <tr>
                      <th style={{ width: 104 }}>時刻（JST）</th>
                      <th>内容</th>
                      <th>結果</th>
                    </tr>
                  </thead>
                  <tbody>
                    {list.map((t) => (
                      <tr key={t.id}>
                        <td className="muted">{timeOf(t.started_at)}</td>
                        <td>{whatOf(role, t)}</td>
                        <td>
                          <Summary role={role} test={t} />
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            ) : (
              <p className="muted" style={{ margin: 0 }}>
                {list ? 'まだ結果はありません。' : '読み込み中…'}
              </p>
            )}
          </div>
        </section>
      </div>
    </Shell>
  );
}

// ---------------------------------------------------------------- parts

function Progress({ value, label, note }: { value: number; label: string; note?: string }) {
  const pct = Math.round(Math.min(1, Math.max(0, value)) * 100);
  return (
    <div className="nt-progress">
      <div className="progress" role="progressbar" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct}>
        <div style={{ width: `${pct}%` }} />
      </div>
      {note && <p>{note}</p>}
    </div>
  );
}

function ProblemLine({ text, alert }: { text: string; alert?: boolean }) {
  return (
    <p className="nt-line nt-problem" role={alert ? 'alert' : undefined}>
      <Glyph tone="err" />
      <span>{text}</span>
    </p>
  );
}

function StateCell({ level }: { level: ReachLevel }) {
  const tone = LEVEL_TONE[level];
  return (
    <span className="nt-state" style={{ color: toneText[tone] }}>
      <Glyph tone={tone} />
      {reachLabel(level)}
    </span>
  );
}

function ReachTable({ role, result }: { role: Role; result: ReachabilityResult }) {
  const noPeer = !result.targets.some((t) => t.kind === 'peer');
  return (
    <div role="status">
      {result.targets.length > 0 && (
        <div className="nt-table-wrap">
          <table className="table nt-table">
            <thead>
              <tr>
                <th>相手</th>
                <th>アドレス</th>
                <th>状態</th>
                <th>往復時間</th>
                <th>届いた数</th>
              </tr>
            </thead>
            <tbody>
              {result.targets.map((t, i) => (
                <tr key={`${t.kind}-${t.address}-${i}`}>
                  <td style={{ fontWeight: 600 }}>{targetName(role, t)}</td>
                  <td>{t.address}</td>
                  <td>
                    <StateCell level={reachLevel(t)} />
                  </td>
                  <td>
                    <Rtt t={t} />
                  </td>
                  <td>
                    {t.received}/{t.sent}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {noPeer && (
        <p className="nt-line">
          <Glyph tone="info" />
          <span>{reachNoPeer(role)}</span>
        </p>
      )}
    </div>
  );
}

function Rtt({ t }: { t: ReachabilityTarget }) {
  if (t.rtt_avg_ms === null) return <span className="muted">—</span>;
  const range =
    t.rtt_min_ms !== null && t.rtt_max_ms !== null ? `（${ms(t.rtt_min_ms)}〜${ms(t.rtt_max_ms)}）` : '';
  return (
    <span className="nowrap-phrase">
      {ms(t.rtt_avg_ms)} ms<span className="muted">{range}</span>
    </span>
  );
}

function TputView({ role, test, bitrateRange }: { role: Role; test: Tput; bitrateRange: [number, number] }) {
  const p = paramsOf(test);
  const what = `${directionText(role, p.direction)}・${testModeShort(p.mode, p.rate)}・${p.duration} 秒`;
  if (test.state === 'running') {
    const r = test.result;
    const latest = r && r.per_second_mbps.length > 0 ? r.per_second_mbps[r.per_second_mbps.length - 1] : null;
    const elapsed = Math.min(p.duration, Math.floor(test.progress * p.duration));
    return (
      <Progress
        value={test.progress}
        label="回線の速さを測っている進み具合"
        note={`${what}　${elapsed} / ${p.duration} 秒${latest !== null ? `　いま ${fixed1(latest)} Mbps` : ''}`}
      />
    );
  }
  if (test.state === 'failed' || !test.result) {
    return <ProblemLine text={throughputErrorText(role, test.error ?? 'failed')} />;
  }
  return <TputResult what={what} result={test.result} direction={p.direction} mode={p.mode} bitrateRange={bitrateRange} />;
}

function TputResult({
  what,
  result: r,
  direction,
  mode,
  bitrateRange,
}: {
  what: string;
  result: ThroughputResult;
  direction: NetworkTestDirection;
  mode: NetworkTestMode;
  bitrateRange: [number, number];
}) {
  // The verdict needs the upper limit in the video's direction; a steady test only sends the rate it was given.
  // Judged on the value as shown (one decimal), so "5.0 Mbps" never reads as too slow.
  const verdict = direction === 'to_peer' && mode === 'max' ? videoVerdict(Number(fixed1(r.throughput_mbps)), bitrateRange) : null;
  const extras: [string, string][] =
    mode === 'steady'
      ? [
          ['損失（届かずに失われた割合）', r.loss_pct !== null ? `${fixed1(r.loss_pct)} %` : '—'],
          ['揺らぎ（遅れのばらつき）', r.jitter_ms !== null ? `${fixed1(r.jitter_ms)} ms` : '—'],
        ]
      : r.rtt_ms !== null
        ? [['往復時間（測っている間）', `${ms(r.rtt_ms)} ms`]]
        : [];
  return (
    <div className="nt-result" role="status">
      <p className="nt-what">{what}</p>
      <div className="nt-figures">
        <div>
          <div className="readout-label">送れた速さ</div>
          <div className="readout-value">
            <b>{fixed1(r.throughput_mbps)}</b>
            <span>Mbps</span>
          </div>
        </div>
        {extras.length > 0 && (
          <dl className="kv">
            {extras.map(([k, v]) => (
              <Fragment key={k}>
                <dt>{k}</dt>
                <dd>{v}</dd>
              </Fragment>
            ))}
          </dl>
        )}
      </div>
      {verdict && (
        <p className="nt-line nt-verdict" style={{ color: toneText[VERDICT_TONE[verdict.level]] }}>
          <Glyph tone={VERDICT_TONE[verdict.level]} />
          <span>{verdict.text}</span>
        </p>
      )}
    </div>
  );
}

/** 内容 column of the list. */
function whatOf(role: Role, t: NetworkTest): string {
  if (isReach(t)) return 'つながりの確認';
  const p = paramsOf(t);
  return `回線の速さ（${directionText(role, p.direction)}・${testModeShort(p.mode, p.rate)}・${p.duration} 秒）`;
}

/** 結果 column of the list: the shape for problems, plain numbers otherwise. */
function Summary({ role, test: t }: { role: Role; test: NetworkTest }) {
  if (t.state === 'running') return <span className="muted">{isReach(t) ? '確かめています…' : '測っています…'}</span>;
  if (isReach(t)) {
    if (t.state === 'failed' || !t.result) return <Flag tone="err" text="確かめられませんでした" />;
    const targets = t.result.targets;
    if (targets.length === 0) return <Flag tone="info" text={reachNoPeer(role)} />;
    const bad = targets.filter((x) => reachLevel(x) !== 'ok');
    if (bad.length === 0) return <Flag tone="ok" text={`すべて届く（${targets.length} か所）`} />;
    const worst: ReachLevel = bad.some((x) => reachLevel(x) === 'none') ? 'none' : 'partial';
    return (
      <Flag
        tone={LEVEL_TONE[worst]}
        text={bad.map((x) => `${targetName(role, x)} ${reachLabel(reachLevel(x))}`).join('、')}
      />
    );
  }
  if (t.state === 'failed' || !t.result) return <Flag tone="err" text={throughputErrorShort(role, t.error ?? 'failed')} />;
  const r = t.result;
  const steady = paramsOf(t).mode === 'steady';
  const parts = [`${fixed1(r.throughput_mbps)} Mbps`];
  if (steady && r.loss_pct !== null) parts.push(`損失 ${fixed1(r.loss_pct)} %`);
  if (steady && r.jitter_ms !== null) parts.push(`揺らぎ ${fixed1(r.jitter_ms)} ms`);
  return <span>{parts.join('、')}</span>;
}

function Flag({ tone, text }: { tone: Tone; text: string }) {
  return (
    <span className="nt-flag">
      <Glyph tone={tone} />
      <span>{text}</span>
    </span>
  );
}
