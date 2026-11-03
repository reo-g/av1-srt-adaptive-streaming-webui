import { useEffect, useMemo, useState } from 'react';
import { ApiFailure, api } from '../api/client';
import { useDrafts, useDraftState, useLive } from '../api/live';
import type { CheckResult, FieldError as FieldErr, Stats, VideoOptions, VideoSettings, VideoSettingsInput } from '../api/types';
import { Segmented } from '../components/Segmented';
import {
  Applied,
  ApplyDialog,
  DiscardDialog,
  FieldError,
  PendingPanel,
  Row,
  type PendingItem,
} from '../components/Settings';
import { Shell } from '../components/Shell';
import { encodingText, fieldErrorText, stateLabel, videoModeHelp, videoModeName, videoModeShort } from '../i18n/ja';
import { fileName, fixed1, timeOf } from '../lib/format';
import { NotDesigned } from './Placeholders';

/** Form state. Numbers are kept as typed text so a half-typed value is not lost. */
interface VDraft {
  kind: 'camera' | 'file';
  camera: string;
  file: string;
  loop: boolean;
  mode: 'auto' | 'manual';
  /** the fixed bitrate (used in manual mode) */
  bitrate: string;
}

type Key = keyof VDraft;

/** Fallback when the options have not arrived yet (the device sends the real range). */
const BITRATE_RANGE: [number, number] = [1.5, 4.0];

/** The applied state as a draft. In auto mode the fixed bitrate starts from what auto aims at. */
function fromSettings(s: VideoSettings): VDraft {
  const br = s.encode.mode === 'auto' ? s.encode.auto.bitrate_mbps : s.encode.manual.bitrate_mbps;
  return {
    kind: s.input.kind,
    camera: s.input.camera,
    file: s.input.file,
    loop: s.input.loop,
    mode: s.encode.mode,
    bitrate: br.toFixed(1),
  };
}

const num = (t: string) => (/^\d+(\.\d+)?$/.test(t.trim()) ? parseFloat(t) : NaN);

function toInput(d: VDraft, base: VideoSettings): VideoSettingsInput {
  const br = num(d.bitrate);
  return {
    version: base.version,
    input: { kind: d.kind, camera: d.camera, file: d.file, loop: d.loop },
    encode: {
      mode: d.mode,
      // An unparsable value is sent as-is (NaN → null) so the device reports it.
      manual: { bitrate_mbps: Number.isFinite(br) ? br : (null as unknown as number) },
    },
  };
}

/** Signal of the input that is selected in the draft (camera signal or file format). */
const inputSignal = (d: VDraft, o: VideoOptions | null): string | null =>
  d.kind === 'camera'
    ? (o?.cameras.find((c) => c.device === d.camera)?.signal ?? null)
    : (o?.files.find((f) => f.path === d.file)?.format ?? null);

/** The camera as the operator knows it (its name, not the device path). */
const cameraName = (o: VideoOptions | null, dev: string) => o?.cameras.find((c) => c.device === dev)?.label ?? fileName(dev);

/** "目標 3.5 Mbps／実際 3.3 Mbps", updated with every stats message. */
const qualityNow = (st: Stats | null, live: boolean) => {
  if (!live || !st || st.target_mbps === null) return null;
  return `目標 ${fixed1(st.target_mbps)} Mbps／実際 ${st.actual_mbps !== null ? `${fixed1(st.actual_mbps)} Mbps` : '—'}`;
};

export function VideoSettingsPage() {
  const { device, status } = useLive();
  if (device && device.role === 'rx') {
    return <NotDesigned route="video" title="映像設定" note="受信機の映像設定（出力先や出力形式）はまだデザインしていません。" />;
  }
  return <VideoSettingsTx statusLabel={device && status ? stateLabel(device.role, status.state) : ''} deviceId={device?.id ?? ''} peerId={status?.peer.id ?? ''} live={status?.state === 'live'} />;
}

function VideoSettingsTx({ statusLabel, deviceId, peerId, live }: { statusLabel: string; deviceId: string; peerId: string; live: boolean }) {
  const { stats } = useLive();
  const { setCount } = useDrafts();
  const [settings, setSettings] = useState<VideoSettings | null>(null);
  const [options, setOptions] = useState<VideoOptions | null>(null);
  const [stored, setStored] = useDraftState<VDraft>('video', null);
  const [check, setCheck] = useState<CheckResult | null>(null);
  const [phase, setPhase] = useState<'edit' | 'confirm' | 'applying' | 'discard'>('edit');
  const [doneAt, setDoneAt] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  /** Bumped when the confirmation opens, so its impacts are checked against the line as it is now. */
  const [recheck, setRecheck] = useState(0);

  useEffect(() => {
    api.video.get().then(setSettings).catch(() => undefined);
    api.video.options().then(setOptions).catch(() => undefined);
  }, []);

  const applied = useMemo(() => (settings ? fromSettings(settings) : null), [settings]);
  const v = stored ?? applied;
  // What auto mode aims at right now (live), for "固定する画質" when leaving auto.
  const autoNow = stats?.target_mbps ?? settings?.encode.auto.bitrate_mbps ?? null;
  // Whether the line capacity is known now (it is only measurable while the line is crowded).
  const capacityKnown = live && stats?.bandwidth_mbps != null;

  const pending: PendingItem[] = useMemo(() => {
    if (!settings || !applied || !v) return [];
    const fromAuto = applied.mode === 'auto';
    const label: Record<Key, [string, (d: VDraft) => string]> = {
      kind: ['入力の種類', (d) => (d.kind === 'camera' ? 'カメラ' : '映像ファイル')],
      camera: ['デバイス', (d) => cameraName(options, d.camera)],
      file: ['ファイル', (d) => fileName(d.file)],
      loop: ['繰り返し再生', (d) => (d.loop ? 'する' : 'しない')],
      mode: ['画質の決め方', (d) => videoModeName(d.mode)],
      bitrate: ['固定する画質', (d) => `${d.bitrate} Mbps`],
    };
    const keys: Key[] = ['kind', v.kind === 'camera' ? 'camera' : 'file', ...(v.kind === 'file' ? (['loop'] as Key[]) : []), 'mode'];
    if (v.mode === 'manual') keys.push('bitrate');
    const same = (k: Key) => {
      // Leaving auto always fixes a bitrate, so it is always listed.
      if (k === 'bitrate') return !fromAuto && num(v.bitrate) === num(applied.bitrate);
      return v[k] === applied[k];
    };
    return keys
      .filter((k) => !same(k))
      .map((k) => {
        const [name, show] = label[k];
        return {
          key: k,
          label: name,
          from: k === 'bitrate' && fromAuto ? `${autoNow !== null ? `${fixed1(autoNow)} Mbps` : '—'}（おまかせ）` : show(applied),
          to: show(v),
          revert: () => revert(k),
        };
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [settings, applied, v, options, autoNow]);

  const n = pending.length;
  useEffect(() => setCount('video', n), [n, setCount]);

  // Validation and impacts come from the device (POST …/check), debounced. Some impacts depend on
  // the line (capacity, what auto aims at), so the check also runs again when those change and
  // when the confirmation opens.
  useEffect(() => {
    if (!settings || !v || n === 0) {
      setCheck(null);
      return;
    }
    const t = setTimeout(() => {
      api.video.check(toInput(v, settings)).then(setCheck).catch(() => undefined);
    }, 200);
    return () => clearTimeout(t);
  }, [settings, v, n, recheck, autoNow, capacityKnown, live]);

  if (!settings || !applied || !v) {
    return (
      <Shell route="video">
        <p className="muted">読み込み中…</p>
      </Shell>
    );
  }

  const set = (patch: Partial<VDraft>) => {
    setDoneAt(null);
    setNotice(null);
    setStored({ ...v, ...patch });
  };
  function revert(k: Key) {
    const next = { ...v!, [k]: applied![k] };
    // While auto is applied, the fixed bitrate only exists together with "画質を固定": revert both.
    if ((k === 'mode' || k === 'bitrate') && applied!.mode === 'auto') {
      next.mode = 'auto';
      next.bitrate = applied!.bitrate;
    }
    setStored(next);
  }
  const changed = (k: Key) => pending.some((p) => p.key === k);
  const errors: FieldErr[] = check?.errors ?? [];
  // Immediate client-side check for unparsable numbers.
  const [brMin, brMax] = options?.bitrate_range_mbps ?? BITRATE_RANGE;
  const errOf = (field: string, text: string, min: number, max: number): FieldErr | undefined => {
    const fromDevice = errors.find((e) => e.field === field);
    if (fromDevice) return fromDevice;
    const x = num(text);
    return v.mode === 'manual' && !(x >= min && x <= max) ? { field, code: 'out_of_range', params: { min, max } } : undefined;
  };
  const brErr = errOf('encode.manual.bitrate_mbps', v.bitrate, brMin, brMax);
  const loopErr = errors.find((e) => e.field === 'input.loop');
  const invalid = !!brErr || !!loopErr;
  const impacts = check?.impacts ?? [];

  const cam = options?.cameras.find((c) => c.device === v.camera);
  const file = options?.files.find((f) => f.path === v.file);
  const signal = inputSignal(v, options);
  const loopOk = options?.loop_available ?? false;
  const quality = qualityNow(stats, live);
  const bandwidth = live ? (stats?.bandwidth_mbps ?? null) : null;

  const apply = async () => {
    setPhase('applying');
    try {
      const res = await api.video.apply(toInput(v, settings));
      setSettings(res.settings);
      setStored(null);
      setDoneAt(timeOf(res.applied_at));
      setPhase('edit');
    } catch (e) {
      setPhase('edit');
      if (e instanceof ApiFailure && e.body?.error.code === 'busy') {
        // The device applies settings only while the transmission is stopped (for now).
        setNotice('伝送中は設定を適用できません。運用画面で伝送を停止してから、もう一度適用してください。');
      } else if (e instanceof ApiFailure && e.status === 409) {
        setNotice('ほかの画面で設定が変わったため、適用しませんでした。最新の値を読み込みました。');
        api.video.get().then(setSettings).catch(() => undefined);
      } else {
        setNotice('適用できませんでした。もう一度お試しください。');
      }
    }
  };

  return (
    <Shell route="video">
      <div className="card">
        <div className="set-head">
          <h1>映像設定</h1>
          <p>
            送信機 {deviceId} は{statusLabel}です。変更は「適用」するまで機器に反映されません。音声はいまは送りません。
          </p>
        </div>
        <div className="set-body">
          <div className="set-form">
            <section aria-labelledby="sec-in">
              <h2 id="sec-in" className="sec-title first">
                入力
              </h2>
              <Row label="入力の種類" labelId="lbl-kind" changed={changed('kind')} padLabel top>
                <Segmented
                  labelledBy="lbl-kind"
                  value={v.kind}
                  onChange={(kind) => set({ kind })}
                  options={[
                    { value: 'camera', label: 'カメラ' },
                    { value: 'file', label: '映像ファイル' },
                  ]}
                />
                {changed('kind') && <Applied value={applied.kind === 'camera' ? 'カメラ' : '映像ファイル'} onRevert={() => revert('kind')} />}
              </Row>
              {v.kind === 'camera' ? (
                <>
                  <Row label="デバイス" labelFor="in-dev" hint="この機器につながっている映像デバイス" changed={changed('camera')}>
                    <select
                      id="in-dev"
                      className={changed('camera') ? 'select wide changed' : 'select wide'}
                      value={v.camera}
                      onChange={(e) => set({ camera: e.target.value })}
                    >
                      {options?.cameras.map((c) => (
                        <option key={c.device} value={c.device}>
                          {c.label}
                        </option>
                      ))}
                    </select>
                    {changed('camera') && <Applied value={cameraName(options, applied.camera)} onRevert={() => revert('camera')} />}
                  </Row>
                  <Row label="検出された入力信号" hint="デバイスから検出した値です（変更できません）">
                    <div className="measured">
                      <span className="chip-outline">実測</span>
                      <b>{cam?.signal ?? '信号なし'}</b>
                    </div>
                  </Row>
                </>
              ) : (
                <>
                  <Row label="ファイル" labelFor="in-file" changed={changed('file') || changed('loop')} invalid={!!loopErr} padLabel top>
                    <select
                      id="in-file"
                      className={changed('file') ? 'select wide changed' : 'select wide'}
                      value={v.file}
                      onChange={(e) => set({ file: e.target.value })}
                    >
                      {options?.files.map((f) => (
                        <option key={f.path} value={f.path}>
                          {fileName(f.path)}
                        </option>
                      ))}
                    </select>
                    <label className={loopOk ? 'check' : 'check disabled'}>
                      <input
                        type="checkbox"
                        checked={loopOk && v.loop}
                        disabled={!loopOk}
                        onChange={(e) => set({ loop: e.target.checked })}
                      />
                      最後まで再生したら最初から繰り返す
                      {!loopOk && <span className="chip-muted">準備中</span>}
                    </label>
                    {!loopOk && <p className="help">いまはファイルを最後まで 1 回だけ送ります。</p>}
                    {loopErr && <FieldError text={fieldErrorText(loopErr)} />}
                    {changed('file') && <Applied value={fileName(applied.file)} onRevert={() => revert('file')} />}
                  </Row>
                  <Row label="ファイルの形式" hint="ファイルから読み取った値です（変更できません）">
                    <div className="measured">
                      <span className="chip-outline">実測</span>
                      <b>
                        {file ? `${file.format}　長さ ${Math.floor(file.duration_s / 60)}:${String(file.duration_s % 60).padStart(2, '0')}` : '—'}
                      </b>
                    </div>
                  </Row>
                </>
              )}
            </section>

            <section aria-labelledby="sec-q">
              <h2 id="sec-q" className="sec-title">
                画質
              </h2>
              <Row label="画質の決め方" labelId="lbl-mode" hint={videoModeHelp(v.mode)} changed={changed('mode')} top>
                <Segmented
                  labelledBy="lbl-mode"
                  value={v.mode}
                  onChange={(mode) => {
                    // Leaving auto starts the fixed bitrate from what auto aims at now; back to auto drops it.
                    if (mode === 'manual' && applied.mode === 'auto' && autoNow !== null) set({ mode, bitrate: autoNow.toFixed(1) });
                    else if (mode === 'auto' && applied.mode === 'auto') set({ mode, bitrate: applied.bitrate });
                    else set({ mode });
                  }}
                  options={[
                    { value: 'auto', label: videoModeName('auto') },
                    { value: 'manual', label: videoModeName('manual') },
                  ]}
                />
                {changed('mode') && <Applied value={videoModeName(applied.mode)} onRevert={() => revert('mode')} />}
              </Row>

              {v.mode === 'manual' && (
                <Row
                  label="固定する画質"
                  labelFor="enc-br"
                  hint={`${fixed1(brMin)}〜${fixed1(brMax)} Mbps（1 秒あたりに送るデータの量）。大きいほどきれいですが、回線に余裕が要ります`}
                  changed={changed('bitrate')}
                  invalid={!!brErr}
                >
                  <div className="inline">
                    <input
                      id="enc-br"
                      type="text"
                      inputMode="decimal"
                      className={changed('bitrate') ? 'input num changed' : 'input num'}
                      value={v.bitrate}
                      aria-invalid={!!brErr}
                      aria-describedby={brErr ? 'enc-br-e' : bandwidth !== null ? 'enc-br-h' : undefined}
                      onChange={(e) => set({ bitrate: e.target.value })}
                    />
                    <span className="unit">Mbps</span>
                  </div>
                  {bandwidth !== null && (
                    <p id="enc-br-h" className="help">
                      いまの回線の推定容量は {fixed1(bandwidth)} Mbps です。これより低めにすると止まりにくくなります。
                    </p>
                  )}
                  {brErr && <FieldError id="enc-br-e" text={fieldErrorText(brErr)} />}
                  {changed('bitrate') && (
                    <Applied value={pending.find((p) => p.key === 'bitrate')!.from} onRevert={() => revert('bitrate')} />
                  )}
                </Row>
              )}

              <Row
                label={
                  <>
                    いまの画質<span className="chip-outline">{videoModeShort(settings.encode.mode)}</span>
                  </>
                }
                hint="1 秒ごとに更新します。目標は機器がめざす値、実際は送っている映像の値です"
                top
              >
                <div className="measured">
                  {quality ? <b>{quality}</b> : <span className="muted">— 伝送していないため、いまの値はありません</span>}
                </div>
              </Row>
              <Row label="映像の形式" hint="変更できません">
                <div className="measured">
                  <span>{options ? encodingText(options.encoding, signal) : '—'}</span>
                </div>
              </Row>
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
            doneNote="新しい設定に切り替えました。"
            emptyNote="未適用の変更はありません。表示中の値はすべて適用済みです。"
          />
        </div>
      </div>

      {(phase === 'confirm' || phase === 'applying') && n > 0 && (
        <ApplyDialog
          items={pending}
          impacts={impacts}
          context={live ? `送信機 ${deviceId} は受信機 ${peerId} へ伝送中です。` : `送信機 ${deviceId} は${statusLabel}です。`}
          applying={phase === 'applying'}
          applyingText="適用しています。新しい設定に切り替えています…"
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
            setPhase('edit');
          }}
        />
      )}
    </Shell>
  );
}
