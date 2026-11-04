import { useEffect, useMemo, useRef, useState } from 'react';
import { ApiFailure, api } from '../api/client';
import { useDrafts, useDraftState, useLive } from '../api/live';
import type {
  CheckResult,
  Cipher,
  CompareResult,
  CompareRow,
  DelayProfile,
  FieldError as FieldErr,
  Status,
  TransportMode,
  TransportOptions,
  TransportSettings,
  TransportSettingsInput,
} from '../api/types';
import { AlertDialog } from '../components/Dialog';
import { Glyph, type Tone } from '../components/Glyph';
import { Segmented } from '../components/Segmented';
import { Applied, ApplyDialog, DiscardDialog, FieldError, PendingPanel, Row, type PendingItem } from '../components/Settings';
import { AlertStrip, Shell } from '../components/Shell';
import {
  cipherName,
  delayDesc,
  delayName,
  fieldErrorText,
  modeName,
  modeSentence,
  sendLimitHelp,
  sendLimitSource,
  sendLimitText,
  stateLabel,
} from '../i18n/ja';
import { fixed2, timeOf } from '../lib/format';
import { href } from '../lib/router';
import { NotDesigned } from './Placeholders';

interface TDraft {
  mode: TransportMode;
  host: string;
  port: string;
  listen_port: string;
  delay: DelayProfile;
  cipher: Cipher;
  /** '' = keep the current passphrase */
  passphrase: string;
  /**
   * 送り出しの上限 as typed, Mbps. null = not edited: the applied value is shown and sent, so a cap
   * set on the network screen meanwhile is not undone by applying another change.
   */
  send_limit: string | null;
}

type Key = 'mode' | 'endpoint' | 'listen_port' | 'delay' | 'cipher' | 'passphrase' | 'send_limit';

/** Range of the send cap; the device reports the same as out_of_range min and max. */
const LIMIT_RANGE: [number, number] = [1, 20];
const round2 = (v: number) => Math.round(v * 100) / 100;
const num = (t: string) => (/^\d+(\.\d+)?$/.test(t.trim()) ? parseFloat(t) : NaN);

const fromSettings = (s: TransportSettings): TDraft => ({
  mode: s.mode,
  host: s.peer.host,
  port: String(s.peer.port),
  listen_port: String(s.listen_port),
  delay: s.delay.profile,
  cipher: s.encryption.cipher,
  passphrase: '',
  send_limit: null,
});

const toInput = (d: TDraft, s: TransportSettings): TransportSettingsInput => ({
  version: s.version,
  mode: d.mode,
  peer: { host: d.host.trim(), port: Number(d.port) },
  listen_port: Number(d.listen_port),
  delay: { profile: d.delay },
  encryption: { cipher: d.cipher, ...(d.passphrase && d.cipher !== 'none' ? { passphrase: d.passphrase } : {}) },
  ...limitInput(d, s),
});

/** The cap as sent: the applied value when not edited, two decimals, an unparsable value as null (the device reports it). */
function limitInput(d: TDraft, s: TransportSettings): Pick<TransportSettingsInput, 'send_limit'> {
  if (!s.send_limit) return {};
  const typed = d.send_limit ?? null;
  if (typed === null) return { send_limit: { mbps: s.send_limit.mbps } };
  const x = num(typed);
  return { send_limit: { mbps: Number.isFinite(x) ? round2(x) : (null as unknown as number) } };
}

const usesPeer = (m: TransportMode) => m !== 'inbound';
const usesListen = (m: TransportMode) => m !== 'outbound';

/** Fallbacks until the options arrive; the device sends what can really be chosen. */
const FALLBACK_OPTIONS: TransportOptions = {
  modes: ['outbound'],
  ciphers: ['none'],
  delay_profiles: [
    { profile: 'standard', target_ms: 9500, available: true },
    { profile: 'low', target_ms: 1500, available: false },
  ],
};

export function TransportSettingsPage() {
  const { device } = useLive();
  if (device && device.role === 'rx') {
    return <NotDesigned route="transport" title="伝送設定" note="受信機の伝送設定（待ち受けポートやパスフレーズ）はまだデザインしていません。" />;
  }
  return <TransportSettingsTx />;
}

function ErrorStrip({ status, onRetry, onAskStop }: { status: Status; onRetry: () => void; onAskStop: () => void }) {
  const rejected = status.alerts.some((a) => a.code === 'peer_rejected_passphrase');
  const lines = rejected
    ? ['受信機が接続を拒否しています。', 'パスフレーズが受信機と一致していません。']
    : ['受信機に接続できません。', 'ケーブルと受信機の電源を確認してください。'];
  return (
    <section role="status" aria-label="現在の状況" className="strip strip-lg tone-err">
      <div style={{ flex: '1 1 520px', minWidth: 0 }}>
        <div className="meta">
          <span className="strip-label">
            <Glyph tone="err" size={12} />
            接続エラー
          </span>
          {status.next_retry_s !== null && (
            <span>
              次の試行まで <b>{status.next_retry_s} 秒</b>
            </span>
          )}
          {status.last_live_until && (
            <span>
              前回の接続 <b>{timeOf(status.last_live_until)} まで</b>
            </span>
          )}
        </div>
        <p className="big">
          {lines.map((l) => (
            <span key={l} className="nowrap-phrase">
              {l}
            </span>
          ))}
        </p>
      </div>
      <div className="inline">
        <button type="button" className="btn" onClick={onRetry}>
          今すぐ再試行
        </button>
        <button type="button" className="btn btn-primary" onClick={onAskStop}>
          <svg width="9" height="9" viewBox="0 0 12 12" aria-hidden="true">
            <rect x="1" y="1" width="10" height="10" fill="currentColor" />
          </svg>
          伝送を停止…
        </button>
      </div>
    </section>
  );
}

function StoppedStrip({ status, onStart }: { status: Status; onStart: () => void }) {
  return (
    <section role="status" aria-label="現在の状況" className="strip tone-off">
      <p>
        <span className="strip-label">
          <Glyph tone="off" size={12} />
          停止中
        </span>
        <span className="strip-text">{timeOf(status.since)} に伝送を停止しました。再接続も止まっています。</span>
      </p>
      <button type="button" className="btn btn-primary" onClick={onStart}>
        伝送を開始
      </button>
    </section>
  );
}

function verdict(tone: Tone, text: string, sub?: string, strong?: boolean) {
  return (
    <span role="cell">
      <span style={{ display: 'inline-flex', alignItems: 'center', gap: 8, fontWeight: strong ? 600 : 400, color: strong ? 'var(--err-text)' : undefined }}>
        <Glyph tone={tone} />
        {text}
      </span>
      {sub && <small>{sub}</small>}
    </span>
  );
}

function CompareTable({
  data,
  localId,
  targetMs,
  cipherLabel,
}: {
  data: CompareResult | null;
  localId: string;
  targetMs: (p: DelayProfile) => number | undefined;
  cipherLabel: string;
}) {
  // Without encryption there is no passphrase to compare.
  const plain = data?.rows.some((r) => r.key === 'cipher' && r.local === 'none') ?? false;
  const rows = (data?.rows ?? []).filter((r) => !(plain && r.key === 'passphrase'));
  const cell = (main: string, sub?: string) => (
    <span role="cell">
      {main}
      {sub && <small>{sub}</small>}
    </span>
  );
  const unknown = verdict('na', '未取得');
  const render = (r: CompareRow) => {
    switch (r.key) {
      case 'mode':
        return (
          <div role="row" className="cmp-row" key={r.key}>
            <span role="cell" className="muted">接続方法</span>
            {cell(modeName(r.local))}
            <span aria-hidden="true" style={{ color: 'var(--muted)' }}>→</span>
            {cell(r.peer ? modeName(r.peer) : '—')}
            {r.verdict === 'compatible'
              ? verdict('ok', '組み合わせできます')
              : r.verdict === 'incompatible'
                ? verdict('err', '組み合わせできません', undefined, true)
                : unknown}
          </div>
        );
      case 'endpoint':
        return (
          <div role="row" className="cmp-row" key={r.key}>
            <span role="cell" className="muted">接続先とポート</span>
            {cell(`${r.local.host}:${r.local.port} へ接続`)}
            <span aria-hidden="true" style={{ color: 'var(--muted)' }}>→</span>
            {cell(r.peer ? `ポート ${r.peer.listen_port} で待ち受け` : '—')}
            {r.verdict === 'match' ? verdict('ok', '一致') : r.verdict === 'mismatch' ? verdict('err', '不一致', undefined, true) : unknown}
          </div>
        );
      case 'delay':
        return (
          <div role="row" className="cmp-row" key={r.key}>
            <span role="cell" className="muted">遅延</span>
            {cell(delayName(r.local, targetMs(r.local)))}
            <span aria-hidden="true" />
            {cell(r.peer ? delayName(r.peer, targetMs(r.peer)) : '—')}
            {r.verdict === 'match'
              ? verdict('ok', '一致')
              : r.verdict === 'mismatch'
                ? verdict('err', '不一致', '受信機と同じにしてください', true)
                : unknown}
          </div>
        );
      case 'cipher':
        return (
          <div role="row" className="cmp-row" key={r.key}>
            <span role="cell" className="muted">{cipherLabel}</span>
            {cell(cipherName(r.local))}
            <span aria-hidden="true" />
            {cell(r.peer ? cipherName(r.peer) : '—')}
            {r.verdict === 'match' ? verdict('ok', '一致') : r.verdict === 'mismatch' ? verdict('err', '不一致', undefined, true) : unknown}
          </div>
        );
      case 'passphrase':
        return (
          <div role="row" className={r.verdict === 'mismatch' ? 'cmp-row bad' : 'cmp-row'} key={r.key}>
            <span role="cell" className="muted">パスフレーズ</span>
            {cell(r.local_set ? '設定済み' : '未設定', '値は表示しません')}
            <span aria-hidden="true" />
            <span role="cell">
              <span className="muted">—</span>
              <small>受信機からは送られません</small>
            </span>
            {r.verdict === 'mismatch'
              ? verdict('err', '不一致', r.reason === 'peer_rejected' ? '受信機の拒否理由より' : undefined, true)
              : r.verdict === 'match'
                ? verdict('ok', '一致', '接続できているため')
                : verdict('na', '未確認')}
          </div>
        );
    }
  };
  return (
    <section aria-labelledby="sec-cmp">
      <div style={{ display: 'flex', flexWrap: 'wrap', justifyContent: 'space-between', alignItems: 'baseline', gap: '4px 24px', padding: '24px 28px 0' }}>
        <h2 id="sec-cmp" style={{ margin: 0, fontSize: 16, fontWeight: 600 }}>
          送信機と受信機の設定を比べる
        </h2>
        <span style={{ fontSize: 13 }} className="muted">
          {data?.received_at ? `受信機の値は接続時（${timeOf(data.received_at)}）に受け取ったものです` : '受信機の値は接続時に受け取ったものです'}
        </span>
      </div>
      <div className="cmp-wrap">
        <div role="table" aria-labelledby="sec-cmp" className="cmp">
          <div role="row" className="cmp-row head">
            <span role="columnheader" style={{ fontSize: 13 }} className="muted">項目</span>
            <span role="columnheader" style={{ fontWeight: 600 }}>送信機 {localId}</span>
            <span aria-hidden="true" />
            <span role="columnheader" style={{ fontWeight: 600 }}>受信機 {data?.peer_id ?? ''}</span>
            <span role="columnheader" style={{ fontSize: 13 }} className="muted">照合</span>
          </div>
          {rows.map(render)}
        </div>
      </div>
    </section>
  );
}

function TransportSettingsTx() {
  const { device, status, setStatus } = useLive();
  const { setCount } = useDrafts();
  const [settings, setSettings] = useState<TransportSettings | null>(null);
  const [options, setOptions] = useState<TransportOptions | null>(null);
  const [compare, setCompare] = useState<CompareResult | null>(null);
  const [stored, setStored] = useDraftState<TDraft>('transport', null);
  const [check, setCheck] = useState<CheckResult | null>(null);
  const [phase, setPhase] = useState<'edit' | 'confirm' | 'applying' | 'discard' | 'stop'>('edit');
  const [doneAt, setDoneAt] = useState<string | null>(null);
  const [doneNote, setDoneNote] = useState('');
  const [showPw, setShowPw] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  /** Bumped when the confirmation opens, so its impacts match the link state at that moment. */
  const [recheck, setRecheck] = useState(0);
  const pwRef = useRef<HTMLInputElement>(null);

  const reload = () => {
    api.transport.get().then(setSettings).catch(() => undefined);
    api.transport.compare().then(setCompare).catch(() => undefined);
  };
  useEffect(() => {
    reload();
    api.transport.options().then(setOptions).catch(() => undefined);
  }, []);
  // The comparison changes when the link state changes.
  const linkState = status?.state;
  useEffect(() => {
    api.transport.compare().then(setCompare).catch(() => undefined);
    api.transport.get().then(setSettings).catch(() => undefined);
  }, [linkState]);

  const applied = useMemo(() => (settings ? fromSettings(settings) : null), [settings]);
  const v = stored ?? applied;
  const opts = options ?? FALLBACK_OPTIONS;
  const targetMs = (p: DelayProfile) => opts.delay_profiles.find((d) => d.profile === p)?.target_ms;
  // While only one cipher can be chosen the row is plain text, named 「暗号化」 everywhere on the screen.
  const singleCipher = opts.ciphers.length === 1 && (!v || opts.ciphers[0] === v.cipher);
  const cipherLabel = singleCipher ? '暗号化' : '暗号化方式';

  const pending: PendingItem[] = useMemo(() => {
    if (!settings || !applied || !v) return [];
    const items: { key: Key; label: string; changed: boolean; from: string; to: string }[] = [
      { key: 'mode', label: '接続方法', changed: v.mode !== applied.mode, from: modeName(applied.mode), to: modeName(v.mode) },
      {
        key: 'endpoint',
        label: '接続先',
        changed: usesPeer(v.mode) && (v.host.trim() !== applied.host || Number(v.port) !== Number(applied.port)),
        from: `${applied.host}:${applied.port}`,
        to: `${v.host.trim()}:${v.port}`,
      },
      {
        key: 'listen_port',
        label: '待ち受けポート',
        changed: usesListen(v.mode) && Number(v.listen_port) !== Number(applied.listen_port),
        from: applied.listen_port,
        to: v.listen_port,
      },
      {
        key: 'delay',
        label: '遅延',
        changed: v.delay !== applied.delay,
        from: delayName(applied.delay, targetMs(applied.delay)),
        to: delayName(v.delay, targetMs(v.delay)),
      },
      { key: 'cipher', label: cipherLabel, changed: v.cipher !== applied.cipher, from: cipherName(applied.cipher), to: cipherName(v.cipher) },
      {
        key: 'passphrase',
        label: 'パスフレーズ',
        changed: v.cipher !== 'none' && v.passphrase !== '',
        from: settings.encryption.passphrase_set ? '設定済み' : '未設定',
        to: '新しい値（非表示）',
      },
    ];
    const limit = settings.send_limit ?? null;
    const typed = v.send_limit ?? null;
    if (limit && typed !== null) {
      const x = num(typed);
      items.push({
        key: 'send_limit',
        label: '送り出しの上限',
        changed: !(Number.isFinite(x) && round2(x) === round2(limit.mbps)),
        from: sendLimitText(limit, true),
        to: Number.isFinite(x) ? `${fixed2(round2(x))} Mbps` : `${typed} Mbps`,
      });
    }
    return items.filter((i) => i.changed).map((i) => ({ key: i.key, label: i.label, from: i.from, to: i.to, revert: () => revert(i.key) }));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [settings, applied, v, options, cipherLabel]);

  const n = pending.length;
  useEffect(() => setCount('transport', n), [n, setCount]);

  useEffect(() => {
    if (!settings || !v || n === 0) {
      setCheck(null);
      return;
    }
    const t = setTimeout(() => {
      api.transport.check(toInput(v, settings)).then(setCheck).catch(() => undefined);
    }, 200);
    return () => clearTimeout(t);
    // The impacts depend on the link state (live, stopped, not connected) as well as the draft.
  }, [settings, v, n, recheck, linkState]);

  if (!device || !status || !settings || !applied || !v) {
    return (
      <Shell route="transport" banner={null}>
        <p className="muted">読み込み中…</p>
      </Shell>
    );
  }

  const set = (patch: Partial<TDraft>) => {
    setDoneAt(null);
    setNotice(null);
    setStored({ ...v, ...patch });
  };
  function revert(k: Key) {
    const a = applied!;
    const cur = v!;
    const back: Partial<TDraft> =
      k === 'endpoint'
        ? { host: a.host, port: a.port }
        : k === 'passphrase'
            ? { passphrase: '' }
            : k === 'send_limit'
              ? { send_limit: null }
              : { [k]: (a as unknown as Record<string, string>)[k] };
    setStored({ ...cur, ...back });
  }
  const changed = (k: Key) => pending.some((p) => p.key === k);
  const errors = check?.errors ?? [];
  const errFor = (field: string) => errors.find((e) => e.field === field);
  const impacts = check?.impacts ?? [];
  const limit = settings.send_limit ?? null;
  const limitTyped = v.send_limit ?? null;
  // An unparsable or out-of-range cap is flagged at once, before the device answers.
  const limitErr: FieldErr | undefined =
    errFor('send_limit.mbps') ??
    (limitTyped !== null && !(num(limitTyped) >= LIMIT_RANGE[0] && num(limitTyped) <= LIMIT_RANGE[1])
      ? { field: 'send_limit.mbps', code: 'out_of_range', params: { min: LIMIT_RANGE[0], max: LIMIT_RANGE[1] } }
      : undefined);
  const invalid = errors.length > 0 || !!limitErr;
  // Only the cap changes: nothing is set up again, the next start uses it (allowed while live).
  const capOnly = n > 0 && pending.every((p) => p.key === 'send_limit');

  const erroring = (status.state === 'reconnecting' || status.state === 'connecting') && status.alerts.some((a) => a.level === 'error');
  const stopped = status.state === 'stopped';
  const rejectedAt = settings.encryption.passphrase_rejected_at;
  // A control with a single choice is shown as text (and the current value is kept even if it is not offered).
  const modeChoices = opts.modes.includes(v.mode) ? opts.modes : [v.mode, ...opts.modes];
  const cipherChoices = opts.ciphers.includes(v.cipher) ? opts.ciphers : [v.cipher, ...opts.ciphers];

  const run = async (f: () => Promise<Status>) => setStatus(await f());

  const apply = async () => {
    setPhase('applying');
    try {
      const res = await api.transport.apply(toInput(v, settings));
      setSettings(res.settings);
      setStored(null);
      setShowPw(false);
      setDoneAt(timeOf(res.applied_at));
      setDoneNote(capOnly ? '送り出しの上限は、次に伝送を始めたときから使います。' : '新しい設定で接続をやり直しています。');
      setPhase('edit');
      api.transport.compare().then(setCompare).catch(() => undefined);
    } catch (e) {
      setPhase('edit');
      if (e instanceof ApiFailure && e.body?.error.code === 'busy') {
        // The device applies settings only while the transmission is stopped (for now); the cap alone is the exception.
        setNotice(
          '伝送中は設定を適用できません。運用画面で伝送を停止してから、もう一度適用してください。' +
            (changed('send_limit') ? '送り出しの上限だけなら、伝送中でも適用できます。' : ''),
        );
      } else if (e instanceof ApiFailure && e.status === 409) {
        setNotice('ほかの画面で設定が変わったため、適用しませんでした。最新の値を読み込みました。');
        reload();
      } else {
        setNotice('適用できませんでした。入力内容を確認してください。');
      }
    }
  };

  const peer = status.peer.id ?? '';
  return (
    <Shell
      route="transport"
      banner={
        erroring ? (
          <ErrorStrip status={status} onRetry={() => run(api.retry)} onAskStop={() => setPhase('stop')} />
        ) : stopped ? (
          <StoppedStrip status={status} onStart={() => run(api.start)} />
        ) : (
          <AlertStrip route="transport" />
        )
      }
    >
      <div className="card">
        <div className="set-head">
          <h1>伝送設定</h1>
          <p>
            受信機 {peer}（{settings.peer.host}:{settings.peer.port}）へ送ります。変更は「適用」するまで反映されません。
          </p>
        </div>
        <div className="set-body">
          <div className="set-form">
            <CompareTable data={compare} localId={device.id} targetMs={targetMs} cipherLabel={cipherLabel} />

            <section aria-labelledby="sec-conn">
              <h2 id="sec-conn" className="sec-title">
                接続
              </h2>
              {modeChoices.length === 1 ? (
                <Row label="接続方法">
                  <div className="measured">
                    <span>{modeSentence(v.mode)}</span>
                  </div>
                </Row>
              ) : (
                <Row label="接続方法" labelId="lbl-mode" hint={modeSentence(v.mode)} changed={changed('mode')}>
                  <Segmented
                    labelledBy="lbl-mode"
                    value={v.mode}
                    onChange={(mode) => set({ mode })}
                    options={modeChoices.map((m) => ({ value: m, label: modeName(m) }))}
                  />
                  {changed('mode') && <Applied value={modeName(applied.mode)} onRevert={() => revert('mode')} />}
                </Row>
              )}
              {usesPeer(v.mode) && (
                <Row label="接続先" labelFor="tr-host" hint="受信機の IP アドレスとポート" changed={changed('endpoint')} invalid={!!errFor('peer.host') || !!errFor('peer.port')}>
                  <div className="inline">
                    <input
                      id="tr-host"
                      type="text"
                      className={changed('endpoint') ? 'input changed' : 'input'}
                      style={{ width: 200, maxWidth: '100%' }}
                      value={v.host}
                      aria-invalid={!!errFor('peer.host')}
                      onChange={(e) => set({ host: e.target.value })}
                    />
                    <span className="unit">:</span>
                    <input
                      type="text"
                      inputMode="numeric"
                      aria-label="ポート"
                      className={changed('endpoint') ? 'input changed' : 'input'}
                      style={{ width: 96, textAlign: 'right' }}
                      value={v.port}
                      aria-invalid={!!errFor('peer.port')}
                      onChange={(e) => set({ port: e.target.value })}
                    />
                  </div>
                  {errFor('peer.host') && <FieldError text={fieldErrorText(errFor('peer.host')!)} />}
                  {errFor('peer.port') && <FieldError text={fieldErrorText(errFor('peer.port')!)} />}
                  {changed('endpoint') && <Applied value={`${applied.host}:${applied.port}`} onRevert={() => revert('endpoint')} />}
                </Row>
              )}
              {usesListen(v.mode) && (
                <Row label="待ち受けポート" labelFor="tr-listen" hint="この送信機が接続を待つポート" changed={changed('listen_port')} invalid={!!errFor('listen_port')}>
                  <input
                    id="tr-listen"
                    type="text"
                    inputMode="numeric"
                    className={changed('listen_port') ? 'input changed' : 'input'}
                    style={{ width: 96, textAlign: 'right' }}
                    value={v.listen_port}
                    aria-invalid={!!errFor('listen_port')}
                    onChange={(e) => set({ listen_port: e.target.value })}
                  />
                  {errFor('listen_port') && <FieldError text={fieldErrorText(errFor('listen_port')!)} />}
                  {changed('listen_port') && <Applied value={applied.listen_port} onRevert={() => revert('listen_port')} />}
                </Row>
              )}
            </section>

            <section aria-labelledby="sec-lat">
              <h2 id="sec-lat" className="sec-title">
                遅延
              </h2>
              <Row
                label="遅延"
                labelId="lbl-delay"
                hint="送信機に入った映像が受信側に映るまでの時間です。長いほど回線の乱れに強くなります"
                changed={changed('delay')}
                invalid={!!errFor('delay.profile')}
                top
              >
                <div role="radiogroup" aria-labelledby="lbl-delay" className="choices">
                  {opts.delay_profiles.map((d) => {
                    const on = v.delay === d.profile;
                    return (
                      <label key={d.profile} className={['choice', on ? 'on' : '', d.available ? '' : 'disabled'].filter(Boolean).join(' ')}>
                        <input
                          type="radio"
                          name="tr-delay"
                          value={d.profile}
                          checked={on}
                          disabled={!d.available}
                          onChange={() => set({ delay: d.profile })}
                        />
                        <span>
                          <b>{delayName(d.profile, d.target_ms)}</b> — {delayDesc(d.profile)}
                          {!d.available && <span className="chip-muted">準備中</span>}
                        </span>
                      </label>
                    );
                  })}
                </div>
                {errFor('delay.profile') && <FieldError text={fieldErrorText(errFor('delay.profile')!)} />}
                {changed('delay') && <Applied value={delayName(applied.delay, targetMs(applied.delay))} onRevert={() => revert('delay')} />}
              </Row>
            </section>

            {limit && (
              <section aria-labelledby="sec-limit">
                <h2 id="sec-limit" className="sec-title">
                  送り出し
                </h2>
                <Row
                  label="送り出しの上限"
                  labelFor="tr-limit"
                  hint={sendLimitHelp}
                  changed={changed('send_limit')}
                  invalid={!!limitErr}
                  top
                >
                  <div className="inline">
                    <input
                      id="tr-limit"
                      type="text"
                      inputMode="decimal"
                      className={changed('send_limit') ? 'input num changed' : 'input num'}
                      value={limitTyped ?? fixed2(limit.mbps)}
                      aria-invalid={!!limitErr}
                      aria-describedby={limitErr ? 'tr-limit-e' : changed('send_limit') ? undefined : 'tr-limit-src'}
                      onChange={(e) => set({ send_limit: e.target.value })}
                    />
                    <span className="unit">Mbps</span>
                    {!changed('send_limit') && (
                      <span id="tr-limit-src" className="muted">
                        {sendLimitSource(limit)}
                      </span>
                    )}
                  </div>
                  {limitErr && <FieldError id="tr-limit-e" text={fieldErrorText(limitErr)} />}
                  {changed('send_limit') && <Applied value={sendLimitText(limit)} onRevert={() => revert('send_limit')} />}
                  <p className="help">
                    <a href={href('network')} style={{ fontWeight: 600 }}>
                      回線の速さを測る
                    </a>
                  </p>
                </Row>
              </section>
            )}

            <section aria-labelledby="sec-enc">
              <h2 id="sec-enc" className="sec-title">
                暗号化
              </h2>
              {cipherChoices.length === 1 ? (
                <Row label={cipherLabel}>
                  <div className="measured">
                    <span>{cipherName(v.cipher)}</span>
                  </div>
                </Row>
              ) : (
                <Row label={cipherLabel} labelFor="tr-ci" changed={changed('cipher')}>
                  <select
                    id="tr-ci"
                    className={changed('cipher') ? 'select changed' : 'select'}
                    value={v.cipher}
                    onChange={(e) => set({ cipher: e.target.value as Cipher })}
                  >
                    {cipherChoices.map((c) => (
                      <option key={c} value={c}>
                        {cipherName(c)}
                      </option>
                    ))}
                  </select>
                  {changed('cipher') && <Applied value={cipherName(applied.cipher)} onRevert={() => revert('cipher')} />}
                </Row>
              )}
              {v.cipher !== 'none' && (
                <Row
                  label="パスフレーズ"
                  labelFor="tr-pw"
                  hint={`受信機 ${peer} と同じ値にしてください`}
                  changed={changed('passphrase')}
                  invalid={!!errFor('encryption.passphrase')}
                  top
                >
                  <div className="inline">
                    <input
                      ref={pwRef}
                      id="tr-pw"
                      type={showPw ? 'text' : 'password'}
                      autoComplete="off"
                      placeholder={settings.encryption.passphrase_set ? '変更するときだけ入力' : '入力してください'}
                      className={changed('passphrase') ? 'input changed' : 'input'}
                      style={{ width: 260, maxWidth: '100%' }}
                      value={v.passphrase}
                      aria-invalid={!!errFor('encryption.passphrase')}
                      aria-describedby="tr-pw-e"
                      onChange={(e) => set({ passphrase: e.target.value })}
                    />
                    <button type="button" className="btn" style={{ minHeight: 40, padding: '0 14px' }} aria-pressed={showPw} onClick={() => setShowPw(!showPw)}>
                      {showPw ? '隠す' : '表示する'}
                    </button>
                  </div>
                  {errFor('encryption.passphrase') && <FieldError id="tr-pw-e" text={fieldErrorText(errFor('encryption.passphrase')!)} />}
                  {rejectedAt && !errFor('encryption.passphrase') && (
                    <FieldError id="tr-pw-e" text={`適用中の値は ${timeOf(rejectedAt)} に受信機から拒否されました`} />
                  )}
                  {changed('passphrase') && <Applied value="設定済み（値は表示しません）" onRevert={() => revert('passphrase')} />}
                </Row>
              )}
            </section>
            {notice && (
              <p role="alert" style={{ margin: '16px 28px 0', fontWeight: 600, color: 'var(--err-text)' }}>
                {notice}
              </p>
            )}
          </div>

          <PendingPanel
            items={pending}
            impacts={impacts}
            invalid={invalid}
            onDiscard={() => n > 0 && setPhase('discard')}
            onApply={() => {
              if (invalid || n === 0) return;
              setRecheck((x) => x + 1);
              setPhase('confirm');
            }}
            doneAt={doneAt}
            doneNote={doneNote}
            emptyNote={
              rejectedAt
                ? '未適用の変更はありません。適用中のパスフレーズは受信機と一致していません。'
                : '未適用の変更はありません。表示中の値はすべて適用済みです。'
            }
            emptyLink={
              rejectedAt ? (
                <a
                  href="#tr-pw"
                  style={{ display: 'inline-block', marginTop: 8, fontSize: 13, fontWeight: 600 }}
                  onClick={(e) => {
                    e.preventDefault();
                    pwRef.current?.focus();
                  }}
                >
                  パスフレーズを入力する
                </a>
              ) : undefined
            }
          />
        </div>
      </div>

      {(phase === 'confirm' || phase === 'applying') && n > 0 && (
        <ApplyDialog
          items={pending}
          impacts={impacts}
          context={status.state === 'live' ? `送信機 ${device.id} は受信機 ${peer} へ伝送中です。` : `送信機 ${device.id} は${stateLabel('tx', status.state)}です。`}
          applying={phase === 'applying'}
          applyingText={capOnly ? '適用しています…' : '適用しています。接続をやり直しています…'}
          onCancel={() => setPhase('edit')}
          onApply={apply}
        />
      )}
      {phase === 'discard' && n > 0 && (
        <DiscardDialog
          n={n}
          onCancel={() => setPhase('edit')}
          onConfirm={() => {
            setStored(null);
            setShowPw(false);
            setPhase('edit');
          }}
        />
      )}
      {phase === 'stop' && (
        <AlertDialog
          title="伝送を停止しますか"
          description={`受信機 ${peer} への再接続をやめ、送信を止めます。`}
          onCancel={() => setPhase('edit')}
          actions={
            <>
              <button type="button" className="btn" onClick={() => setPhase('edit')} data-autofocus>
                キャンセル
              </button>
              <button
                type="button"
                className="btn btn-danger"
                onClick={async () => {
                  await run(api.stop);
                  setPhase('edit');
                }}
              >
                伝送を停止する
              </button>
            </>
          }
        />
      )}
    </Shell>
  );
}
