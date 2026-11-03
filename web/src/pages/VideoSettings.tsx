import { useEffect, useMemo, useState } from 'react';
import { ApiFailure, api } from '../api/client';
import { useDrafts, useDraftState, useLive } from '../api/live';
import type { CheckResult, EncodeParams, FieldError as FieldErr, VideoOptions, VideoSettings, VideoSettingsInput } from '../api/types';
import { Chevron } from '../components/Glyph';
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
import { fieldErrorText, stateLabel } from '../i18n/ja';
import { timeOf } from '../lib/format';
import { NotDesigned } from './Placeholders';

/** Form state. Numbers are kept as typed text so a half-typed value is not lost. */
interface VDraft {
  kind: 'camera' | 'file';
  camera: string;
  file: string;
  loop: boolean;
  mode: 'auto' | 'manual';
  resolution: EncodeParams['resolution'];
  framerate: EncodeParams['framerate'];
  bitrate: string;
  rate_control: EncodeParams['rate_control'];
  codec: EncodeParams['codec'];
  profile: EncodeParams['profile'];
  keyframe: string;
  bframes: string;
  encoder: EncodeParams['encoder'];
}

type Key = keyof VDraft;
const MANUAL_KEYS: Key[] = ['resolution', 'framerate', 'bitrate', 'rate_control', 'codec', 'profile', 'keyframe', 'bframes', 'encoder'];
const ADVANCED: Key[] = ['codec', 'profile', 'keyframe', 'bframes', 'encoder'];

/** The applied state as a draft. In auto mode the manual fields start from what auto chose. */
function fromSettings(s: VideoSettings): VDraft {
  const p = s.encode.mode === 'auto' ? s.encode.auto : s.encode.manual;
  return {
    kind: s.input.kind,
    camera: s.input.camera,
    file: s.input.file,
    loop: s.input.loop,
    mode: s.encode.mode,
    resolution: p.resolution,
    framerate: p.framerate,
    bitrate: p.bitrate_mbps.toFixed(1),
    rate_control: p.rate_control,
    codec: p.codec,
    profile: p.profile,
    keyframe: p.keyframe_s.toFixed(1),
    bframes: String(p.bframes),
    encoder: p.encoder,
  };
}

const num = (t: string) => (/^\d+(\.\d+)?$/.test(t.trim()) ? parseFloat(t) : NaN);

function toInput(d: VDraft, base: VideoSettings): VideoSettingsInput {
  const br = num(d.bitrate);
  const kf = num(d.keyframe);
  return {
    version: base.version,
    input: { kind: d.kind, camera: d.camera, file: d.file, loop: d.loop },
    encode: {
      mode: d.mode,
      manual: {
        resolution: d.resolution,
        framerate: d.framerate,
        // An unparsable value is sent as-is (NaN → null) so the device reports it.
        bitrate_mbps: Number.isFinite(br) ? br : (null as unknown as number),
        rate_control: d.rate_control,
        codec: d.codec,
        profile: d.profile,
        keyframe_s: Number.isFinite(kf) ? kf : (null as unknown as number),
        bframes: Number(d.bframes) as 0 | 1 | 2,
        encoder: d.encoder,
      },
    },
  };
}

const signalParts = (signal: string | null) => {
  const [res, fps] = (signal ?? '').split('　');
  return { res: res || '—', fps: fps || '—' };
};

export function VideoSettingsPage() {
  const { device, status } = useLive();
  if (device && device.role === 'rx') {
    return <NotDesigned route="video" title="映像設定" note="受信機の映像設定（出力先や出力形式）はまだデザインしていません。" />;
  }
  return <VideoSettingsTx statusLabel={device && status ? stateLabel(device.role, status.state) : ''} deviceId={device?.id ?? ''} peerId={status?.peer.id ?? ''} live={status?.state === 'live'} />;
}

function VideoSettingsTx({ statusLabel, deviceId, peerId, live }: { statusLabel: string; deviceId: string; peerId: string; live: boolean }) {
  const { setCount } = useDrafts();
  const [settings, setSettings] = useState<VideoSettings | null>(null);
  const [options, setOptions] = useState<VideoOptions | null>(null);
  const [stored, setStored] = useDraftState<VDraft>('video', null);
  const [check, setCheck] = useState<CheckResult | null>(null);
  const [phase, setPhase] = useState<'edit' | 'confirm' | 'applying' | 'discard'>('edit');
  const [doneAt, setDoneAt] = useState<string | null>(null);
  const [adv, setAdv] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    api.video.get().then(setSettings).catch(() => undefined);
    api.video.options().then(setOptions).catch(() => undefined);
  }, []);

  const applied = useMemo(() => (settings ? fromSettings(settings) : null), [settings]);
  const v = stored ?? applied;

  const pending: PendingItem[] = useMemo(() => {
    if (!settings || !applied || !v) return [];
    const autoMark = settings.encode.mode === 'auto' ? '（自動）' : '';
    const cam = (dev: string) => {
      const c = options?.cameras.find((x) => x.device === dev);
      return c ? `${c.device}（${c.label}）` : dev;
    };
    const sig = signalParts(options?.cameras.find((x) => x.device === v.camera)?.signal ?? null);
    const label: Record<Key, [string, (d: VDraft) => string]> = {
      kind: ['入力の種類', (d) => (d.kind === 'camera' ? 'カメラ' : '映像ファイル')],
      camera: ['デバイス', (d) => cam(d.camera)],
      file: ['ファイル', (d) => d.file],
      loop: ['繰り返し再生', (d) => (d.loop ? 'する' : 'しない')],
      mode: ['エンコードの設定', (d) => (d.mode === 'auto' ? '自動' : '手動')],
      resolution: ['解像度', (d) => (d.resolution === 'source' ? `入力と同じ（${sig.res}）` : '1280×720')],
      framerate: ['フレームレート', (d) => (d.framerate === 'source' ? `入力と同じ（${sig.fps}）` : '29.97p')],
      bitrate: ['ビットレート', (d) => `${d.bitrate} Mbps`],
      rate_control: ['レート制御', (d) => (d.rate_control === 'cbr' ? 'CBR（固定）' : 'VBR（可変）')],
      codec: ['コーデック', (d) => (d.codec === 'h264' ? 'H.264' : 'H.265')],
      profile: ['プロファイル', (d) => ({ high: 'High', main: 'Main', baseline: 'Baseline' })[d.profile]],
      keyframe: ['キーフレーム間隔', (d) => `${d.keyframe} 秒`],
      bframes: ['B フレーム', (d) => d.bframes],
      encoder: ['エンコーダ', (d) => (d.encoder === 'hardware' ? 'ハードウェア' : 'ソフトウェア')],
    };
    const keys: Key[] = ['kind', v.kind === 'camera' ? 'camera' : 'file', ...(v.kind === 'file' ? (['loop'] as Key[]) : []), 'mode'];
    if (v.mode === 'manual') keys.push(...MANUAL_KEYS);
    const same = (k: Key) => {
      if (k === 'bitrate' || k === 'keyframe') return num(v[k]) === num(applied[k]);
      return v[k] === applied[k];
    };
    return keys
      .filter((k) => !same(k))
      .map((k) => {
        const [name, show] = label[k];
        const isManualField = MANUAL_KEYS.includes(k);
        return {
          key: k,
          label: name,
          from: show(applied) + (isManualField ? autoMark : ''),
          to: show(v),
          revert: () => revert(k),
        };
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [settings, applied, v, options]);

  const n = pending.length;
  useEffect(() => setCount('video', n), [n, setCount]);

  // Validation and impacts come from the device (POST …/check), debounced.
  useEffect(() => {
    if (!settings || !v || n === 0) {
      setCheck(null);
      return;
    }
    const t = setTimeout(() => {
      api.video.check(toInput(v, settings)).then(setCheck).catch(() => undefined);
    }, 200);
    return () => clearTimeout(t);
  }, [settings, v, n]);

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
    // Going back to auto restores every manual field to what auto uses.
    if (k === 'mode' && applied!.mode === 'auto') for (const m of MANUAL_KEYS) (next as Record<string, unknown>)[m] = applied![m];
    setStored(next);
  }
  const changed = (k: Key) => pending.some((p) => p.key === k);
  const errors: FieldErr[] = check?.errors ?? [];
  // Immediate client-side check for unparsable numbers.
  const [brMin, brMax] = options?.bitrate_range_mbps ?? [1, 40];
  const [kfMin, kfMax] = options?.keyframe_range_s ?? [0.5, 10];
  const errOf = (field: string, text: string, min: number, max: number): FieldErr | undefined => {
    const fromDevice = errors.find((e) => e.field === field);
    if (fromDevice) return fromDevice;
    const x = num(text);
    return v.mode === 'manual' && !(x >= min && x <= max) ? { field, code: 'out_of_range', params: { min, max } } : undefined;
  };
  const brErr = errOf('encode.manual.bitrate_mbps', v.bitrate, brMin, brMax);
  const kfErr = errOf('encode.manual.keyframe_s', v.keyframe, kfMin, kfMax);
  const invalid = !!brErr || !!kfErr;
  const impacts = check?.impacts ?? [];

  const cam = options?.cameras.find((c) => c.device === v.camera);
  const file = options?.files.find((f) => f.path === v.file);
  const sig = signalParts(cam?.signal ?? null);
  const advChanged = ADVANCED.filter((k) => changed(k)).length;
  const autoP = settings.encode.auto;

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
      if (e instanceof ApiFailure && e.status === 409) {
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
                          {c.device}（{c.label}）
                        </option>
                      ))}
                    </select>
                    {changed('camera') && <Applied value={applied.camera} onRevert={() => revert('camera')} />}
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
                  <Row label="ファイル" labelFor="in-file" changed={changed('file') || changed('loop')} padLabel top>
                    <select
                      id="in-file"
                      className={changed('file') ? 'select wide changed' : 'select wide'}
                      value={v.file}
                      onChange={(e) => set({ file: e.target.value })}
                    >
                      {options?.files.map((f) => (
                        <option key={f.path} value={f.path}>
                          {f.path}
                        </option>
                      ))}
                    </select>
                    <label style={{ display: 'flex', alignItems: 'center', gap: 8, marginTop: 10 }}>
                      <input
                        type="checkbox"
                        checked={v.loop}
                        onChange={(e) => set({ loop: e.target.checked })}
                        style={{ width: 16, height: 16, margin: 0, accentColor: 'var(--accent)' }}
                      />
                      最後まで再生したら最初から繰り返す
                    </label>
                    {changed('file') && <Applied value={applied.file} onRevert={() => revert('file')} />}
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

            <section aria-labelledby="sec-enc">
              <h2 id="sec-enc" className="sec-title">
                映像エンコード
              </h2>
              <Row
                label="エンコードの設定"
                labelId="lbl-mode"
                hint="自動では、入力と回線の状態に合わせて機器が決めます"
                changed={changed('mode')}
                top
              >
                <Segmented
                  labelledBy="lbl-mode"
                  value={v.mode}
                  onChange={(mode) => {
                    // Back to auto: manual fields return to what auto uses.
                    if (mode === 'auto' && applied.mode === 'auto') {
                      const back: Partial<VDraft> = {};
                      for (const k of MANUAL_KEYS) (back as Record<string, unknown>)[k] = applied[k];
                      set({ ...back, mode });
                    } else set({ mode });
                  }}
                  options={[
                    { value: 'auto', label: '自動' },
                    { value: 'manual', label: '手動' },
                  ]}
                />
                {changed('mode') && <Applied value={applied.mode === 'auto' ? '自動' : '手動'} onRevert={() => revert('mode')} />}
              </Row>

              {v.mode === 'auto' ? (
                <Row
                  label={
                    <>
                      いまの値<span className="chip-outline">自動</span>
                    </>
                  }
                  hint="機器が決めた値です。手動にすると変更できます"
                  top
                >
                  <dl className="kv">
                    <dt>解像度</dt>
                    <dd>{autoP.resolution === 'source' ? `${sig.res}（入力と同じ）` : '1280×720'}</dd>
                    <dt>フレームレート</dt>
                    <dd>{autoP.framerate === 'source' ? `${sig.fps}（入力と同じ）` : '29.97p'}</dd>
                    <dt>ビットレート</dt>
                    <dd>{autoP.bitrate_mbps.toFixed(1)} Mbps（回線に合わせて調整）</dd>
                    <dt>コーデック</dt>
                    <dd>
                      {autoP.codec === 'h264' ? 'H.264' : 'H.265'} {({ high: 'High', main: 'Main', baseline: 'Baseline' } as const)[autoP.profile]}
                    </dd>
                  </dl>
                </Row>
              ) : (
                <div>
                  <Row label="解像度" labelFor="enc-res" changed={changed('resolution')}>
                    <select
                      id="enc-res"
                      className={changed('resolution') ? 'select changed' : 'select'}
                      value={v.resolution}
                      onChange={(e) => set({ resolution: e.target.value as VDraft['resolution'] })}
                    >
                      <option value="source">入力と同じ（{sig.res}）</option>
                      <option value="1280x720">1280×720</option>
                    </select>
                  </Row>
                  <Row label="フレームレート" labelFor="enc-fps" changed={changed('framerate')}>
                    <select
                      id="enc-fps"
                      className={changed('framerate') ? 'select changed' : 'select'}
                      value={v.framerate}
                      onChange={(e) => set({ framerate: e.target.value as VDraft['framerate'] })}
                    >
                      <option value="source">入力と同じ（{sig.fps}）</option>
                      <option value="29.97">29.97p</option>
                    </select>
                  </Row>
                  <Row
                    label="ビットレート"
                    labelFor="enc-br"
                    hint={`${brMin}〜${brMax} Mbps。${options?.bandwidth_mbps ? `回線の推定帯域は ${options.bandwidth_mbps} Mbps` : ''}`}
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
                        aria-describedby={brErr ? 'enc-br-e' : undefined}
                        onChange={(e) => set({ bitrate: e.target.value })}
                      />
                      <span className="unit">Mbps</span>
                    </div>
                    {brErr && <FieldError id="enc-br-e" text={fieldErrorText(brErr)} />}
                    {changed('bitrate') && (
                      <Applied value={pending.find((p) => p.key === 'bitrate')!.from} onRevert={() => revert('bitrate')} />
                    )}
                  </Row>
                  <Row label="レート制御" labelId="lbl-rc" hint="CBR は帯域が一定で、伝送が安定します" changed={changed('rate_control')}>
                    <Segmented
                      labelledBy="lbl-rc"
                      value={v.rate_control}
                      onChange={(rate_control) => set({ rate_control })}
                      options={[
                        { value: 'cbr', label: 'CBR（固定）' },
                        { value: 'vbr', label: 'VBR（可変）' },
                      ]}
                    />
                  </Row>
                  <button type="button" className="adv-toggle" aria-expanded={adv} aria-controls="enc-adv" onClick={() => setAdv(!adv)}>
                    <Chevron />
                    詳細設定（{ADVANCED.length} 項目）
                    {!adv && advChanged > 0 && <span className="chip-changed">変更 {advChanged} 件を含む</span>}
                    <span className="note">通常は変更不要です</span>
                  </button>
                  {adv && (
                    <div id="enc-adv">
                      <Row label="コーデック" labelFor="enc-codec" hint="受信機が対応している形式を選びます" changed={changed('codec')}>
                        <select
                          id="enc-codec"
                          className={changed('codec') ? 'select changed' : 'select'}
                          value={v.codec}
                          onChange={(e) => set({ codec: e.target.value as VDraft['codec'] })}
                        >
                          <option value="h264">H.264</option>
                          <option value="h265">H.265</option>
                        </select>
                      </Row>
                      <Row label="プロファイル" labelFor="enc-prof" changed={changed('profile')}>
                        <select
                          id="enc-prof"
                          className={changed('profile') ? 'select changed' : 'select'}
                          value={v.profile}
                          onChange={(e) => set({ profile: e.target.value as VDraft['profile'] })}
                        >
                          <option value="high">High</option>
                          <option value="main">Main</option>
                          <option value="baseline">Baseline</option>
                        </select>
                      </Row>
                      <Row
                        label="キーフレーム間隔"
                        labelFor="enc-kf"
                        hint={`${kfMin}〜${kfMax} 秒。短いほど乱れから早く回復し、長いほど画質が上がります`}
                        changed={changed('keyframe')}
                        invalid={!!kfErr}
                      >
                        <div className="inline">
                          <input
                            id="enc-kf"
                            type="text"
                            inputMode="decimal"
                            className={changed('keyframe') ? 'input num changed' : 'input num'}
                            value={v.keyframe}
                            aria-invalid={!!kfErr}
                            aria-describedby={kfErr ? 'enc-kf-e' : undefined}
                            onChange={(e) => set({ keyframe: e.target.value })}
                          />
                          <span className="unit">秒</span>
                        </div>
                        {kfErr && <FieldError id="enc-kf-e" text={fieldErrorText(kfErr)} />}
                        {changed('keyframe') && (
                          <Applied value={pending.find((p) => p.key === 'keyframe')!.from} onRevert={() => revert('keyframe')} />
                        )}
                      </Row>
                      <Row label="B フレーム" labelFor="enc-bf" hint="0 にすると遅延が最小になります" changed={changed('bframes')}>
                        <select
                          id="enc-bf"
                          className={changed('bframes') ? 'select changed' : 'select'}
                          value={v.bframes}
                          onChange={(e) => set({ bframes: e.target.value })}
                        >
                          <option value="0">0</option>
                          <option value="1">1</option>
                          <option value="2">2</option>
                        </select>
                      </Row>
                      <Row label="エンコーダ" labelFor="enc-enc" changed={changed('encoder')}>
                        <select
                          id="enc-enc"
                          className={changed('encoder') ? 'select changed' : 'select'}
                          value={v.encoder}
                          onChange={(e) => set({ encoder: e.target.value as VDraft['encoder'] })}
                        >
                          <option value="hardware">ハードウェア</option>
                          <option value="software">ソフトウェア</option>
                        </select>
                      </Row>
                    </div>
                  )}
                </div>
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
            onApply={() => !invalid && n > 0 && setPhase('confirm')}
            doneAt={doneAt}
            doneNote="エンコーダの再初期化が終わり、伝送を続けています。"
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
          applyingText="適用しています。エンコーダを再初期化中です…"
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
