/**
 * ネットワーク: checks of the line to the peer, started from the browser.
 *   つながりの確認  whether the peer and the devices on the way answer (both roles, also while live)
 *   回線の速さ      how fast data arrives when more is sent than the line can carry (transmitter
 *                   only, while the link is stopped); a result towards the receiver can become the
 *                   send cap (送り出しの上限) of the next transmissions
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
  NetworkTestOptions,
  ReachabilityResult,
  ReachabilityTarget,
  Role,
  SendLimit,
  ThroughputResult,
} from '../api/types';
import { AlertDialog } from '../components/Dialog';
import { Glyph, toneText, type Tone } from '../components/Glyph';
import { Segmented } from '../components/Segmented';
import { Row } from '../components/Settings';
import { Shell } from '../components/Shell';
import {
  directionChoice,
  directionText,
  fasterThan,
  fieldErrorText,
  reachFailed,
  reachLabel,
  reachLevel,
  reachNoPeer,
  targetName,
  testStartProblem,
  throughputBlocked,
  throughputErrorShort,
  throughputErrorText,
  throughputHelp,
  throughputNote,
  capButton,
  capConfirmText,
  capConfirmTitle,
  capCurrent,
  capDone,
  capOffer,
  capUsedMark,
  capProblem,
  videoVerdict,
  type ReachLevel,
} from '../i18n/ja';
import { fixed1, fixed2, timeOf } from '../lib/format';
import { href } from '../lib/router';

type Reach = Extract<NetworkTest, { kind: 'reachability' }>;
type Tput = Extract<NetworkTest, { kind: 'throughput' }>;

/** Fallbacks until the device's options arrive. */
const BITRATE_RANGE: [number, number] = [1.5, 4.0];
const SATURATE_MBPS = 6;
const DURATION_CHOICES = [10, 20, 30];
const DEFAULT_DURATION = 10;
/** Range of the send cap (TransportSettings.send_limit.mbps). */
const LIMIT_RANGE: [number, number] = [1, 20];

const LEVEL_TONE: Record<ReachLevel, Tone> = { ok: 'ok', partial: 'warn', none: 'err' };
const VERDICT_TONE = { ok: 'ok', lower: 'warn', short: 'err' } as const;

/** Round-trip time: one decimal below 10 ms (a device next door answers in 0.4 ms). */
const ms = (v: number) => (v < 10 ? v.toFixed(1) : String(Math.round(v)));

/** The request as the device ran it, with the defaults where a field is missing. */
const paramsOf = (t: Tput) => ({
  direction: (t.request?.direction ?? 'to_peer') as NetworkTestDirection,
  duration: t.request?.duration_s ?? DEFAULT_DURATION,
});

const isReach = (t: NetworkTest): t is Reach => t.kind === 'reachability';
const isTput = (t: NetworkTest): t is Tput => t.kind === 'throughput';

const round2 = (v: number) => Math.round(v * 100) / 100;
/** false only when the device said the line took everything (an older result without the flag counts as full). */
const isSaturated = (r: ThroughputResult) => r.saturated !== false;
/** The line's speed as the result gives it: what arrived, or the test's rate when the line took everything. */
const speedOf = (r: ThroughputResult, saturateMbps: number) => round2(isSaturated(r) ? r.throughput_mbps : saturateMbps);

/**
 * What a result would make the send cap (the device does the same in POST …/use), or null when it
 * cannot be one: not towards the receiver, not finished, or slower than the lowest cap.
 */
function capFrom(t: Tput, saturateMbps: number): number | null {
  if (t.state !== 'done' || !t.result || paramsOf(t).direction !== 'to_peer') return null;
  const v = speedOf(t.result, saturateMbps);
  if (v < LIMIT_RANGE[0]) return null;
  return Math.min(LIMIT_RANGE[1], v);
}

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
  const [duration, setDuration] = useState(DEFAULT_DURATION);
  // The send cap now (tx), and making a result the cap
  const [limit, setLimit] = useState<SendLimit | null>(null);
  const [capAsk, setCapAsk] = useState<{ id: string; mbps: number } | null>(null);
  const [capPosting, setCapPosting] = useState(false);
  const [capNotice, setCapNotice] = useState<string | null>(null);

  // A list read that started before a test was posted must not hide the new test.
  const gen = useRef(0);
  const load = useCallback(() => {
    const g = gen.current;
    api.networkTest
      .list()
      .then((r) => g === gen.current && setList(r.items))
      .catch(() => undefined);
  }, []);
  // The cap can also change in the transport settings or on another screen. A read that started
  // before the cap was changed here must not bring the old value back.
  const limitGen = useRef(0);
  const loadLimit = useCallback(() => {
    const g = limitGen.current;
    api.transport
      .get()
      .then((s) => g === limitGen.current && setLimit(s.send_limit ?? null))
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
      loadLimit();
      api.networkTest.options().then(setOptions).catch(() => undefined);
      api.video
        .options()
        .then((o) => setBitrateRange(o.bitrate_range_mbps))
        .catch(() => undefined);
    }
  }, [role, load, loadLimit]);

  // Every second while a test runs (progress), every 5 s otherwise (a test started from another
  // screen shows up). The cap is read again every 5 s as well.
  const running = list?.some((t) => t.state === 'running') ?? false;
  useEffect(() => {
    const t = setInterval(load, running ? 1000 : 5000);
    return () => clearInterval(t);
  }, [running, load]);
  useEffect(() => {
    if (role !== 'tx') return;
    const t = setInterval(loadLimit, 5000);
    return () => clearInterval(t);
  }, [role, loadLimit]);

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

  const saturate = options?.saturate_mbps ?? SATURATE_MBPS;
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
    setCapNotice(null);
    setPosting('throughput');
    try {
      put(await api.networkTest.start({ kind: 'throughput', direction, duration_s: duration }));
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

  const askCap = (t: Tput) => {
    const mbps = capFrom(t, saturate);
    if (mbps === null) return;
    setCapNotice(null);
    setCapAsk({ id: t.id, mbps });
  };
  const confirmCap = async () => {
    if (!capAsk) return;
    setCapPosting(true);
    try {
      const s = await api.networkTest.use(capAsk.id);
      limitGen.current += 1;
      setLimit(s.send_limit);
      setCapAsk(null);
    } catch (e) {
      setCapAsk(null);
      setCapNotice(capProblem(e instanceof ApiFailure ? e.body?.error.code : undefined));
      loadLimit();
    } finally {
      setCapPosting(false);
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
                  {throughputHelp(direction, saturate)}
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
                {tput && <TputView role={role} test={tput} bitrateRange={bitrateRange} saturate={saturate} />}
                {tput && limit && (
                  <CapBox
                    test={tput}
                    limit={limit}
                    cap={capFrom(tput, saturate)}
                    onUse={() => askCap(tput)}
                    busy={capPosting || capAsk !== null}
                  />
                )}
                {capNotice && <ProblemLine text={capNotice} alert />}
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
                          <Summary role={role} test={t} saturate={saturate} used={!!limit && limit.test_id === t.id} />
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

      {capAsk && limit && (
        <AlertDialog
          busy={capPosting}
          title={capConfirmTitle}
          description={capConfirmText(limit.mbps, capAsk.mbps, status.state !== 'stopped')}
          onCancel={() => !capPosting && setCapAsk(null)}
          actions={
            capPosting ? (
              <div role="status" style={{ display: 'flex', alignItems: 'center', minHeight: 44, fontWeight: 600 }}>
                上限を変えています…
              </div>
            ) : (
              <>
                <button type="button" className="btn" onClick={() => setCapAsk(null)} data-autofocus>
                  キャンセル
                </button>
                <button type="button" className="btn btn-primary" onClick={confirmCap}>
                  上限にする
                </button>
              </>
            )
          }
        />
      )}
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

function TputView({
  role,
  test,
  bitrateRange,
  saturate,
}: {
  role: Role;
  test: Tput;
  bitrateRange: [number, number];
  saturate: number;
}) {
  const p = paramsOf(test);
  const what = `${directionText(role, p.direction)}・${p.duration} 秒`;
  if (test.state === 'running') {
    const elapsed = Math.min(p.duration, Math.floor(test.progress * p.duration));
    return <Progress value={test.progress} label="回線の速さを測っている進み具合" note={`${what}　${elapsed} / ${p.duration} 秒`} />;
  }
  if (test.state === 'failed' || !test.result) {
    return <ProblemLine text={throughputErrorText(role, test.error ?? 'failed')} />;
  }
  return <TputResult what={what} result={test.result} direction={p.direction} bitrateRange={bitrateRange} saturate={saturate} />;
}

function TputResult({
  what,
  result: r,
  direction,
  bitrateRange,
  saturate,
}: {
  what: string;
  result: ThroughputResult;
  direction: NetworkTestDirection;
  bitrateRange: [number, number];
  saturate: number;
}) {
  const full = isSaturated(r);
  // The verdict is about the video's direction, judged on the value as shown (two decimals).
  const verdict = direction === 'to_peer' ? videoVerdict(speedOf(r, saturate), bitrateRange) : null;
  const extras: [string, string][] = [];
  if (Number.isFinite(r.sent_mbps)) extras.push(['送った速さ', `${fixed2(r.sent_mbps)} Mbps`]);
  if (full && Number.isFinite(r.loss_pct)) extras.push(['届かなかった割合', `${fixed1(r.loss_pct)} %`]);
  return (
    <div className="nt-result" role="status">
      <p className="nt-what">{what}</p>
      <div className="nt-figures">
        <div>
          <div className="readout-label">届いた速さ</div>
          <div className="readout-value">
            {full ? (
              <>
                <b>{fixed2(r.throughput_mbps)}</b>
                <span>Mbps</span>
              </>
            ) : (
              <b className="nt-atleast">{fasterThan(saturate)}</b>
            )}
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
      <p className="nt-note">{throughputNote(full, Number.isFinite(r.sent_mbps) ? r.sent_mbps : saturate)}</p>
      {verdict && (
        <p className="nt-line nt-verdict" style={{ color: toneText[VERDICT_TONE[verdict.level]] }}>
          <Glyph tone={VERDICT_TONE[verdict.level]} />
          <span>{verdict.text}</span>
        </p>
      )}
    </div>
  );
}

/**
 * Under a finished result towards the receiver: offer to make it the send cap, or say that it is
 * the cap now. Hidden when the result is the cap already (from somewhere else) or cannot be one.
 */
function CapBox({
  test,
  limit,
  cap,
  onUse,
  busy,
}: {
  test: Tput;
  limit: SendLimit;
  cap: number | null;
  onUse: () => void;
  busy: boolean;
}) {
  if (limit.test_id === test.id && limit.source === 'measured') {
    return (
      <p className="nt-line nt-used" role="status">
        <Glyph tone="ok" />
        <span>{capDone(limit)}</span>
      </p>
    );
  }
  if (cap === null || round2(cap) === round2(limit.mbps)) return null;
  return (
    <div className="nt-use">
      <p>
        {capOffer}
        <span className="nowrap-phrase">{capCurrent(limit)}</span>
      </p>
      <button type="button" className="btn btn-primary" onClick={onUse} disabled={busy}>
        {capButton}
      </button>
    </div>
  );
}

/** 内容 column of the list. */
function whatOf(role: Role, t: NetworkTest): string {
  if (isReach(t)) return 'つながりの確認';
  const p = paramsOf(t);
  return `回線の速さ（${directionText(role, p.direction)}・${p.duration} 秒）`;
}

/** 結果 column of the list: the shape for problems, plain numbers otherwise. */
function Summary({ role, test: t, saturate, used }: { role: Role; test: NetworkTest; saturate: number; used: boolean }) {
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
  return (
    <span className="nt-sum">
      <span>届いた速さ {isSaturated(r) ? `${fixed2(r.throughput_mbps)} Mbps` : fasterThan(saturate)}</span>
      {used && <span className="chip-outline">{capUsedMark}</span>}
    </span>
  );
}

function Flag({ tone, text }: { tone: Tone; text: string }) {
  return (
    <span className="nt-flag">
      <Glyph tone={tone} />
      <span>{text}</span>
    </span>
  );
}
