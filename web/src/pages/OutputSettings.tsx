/**
 * 映像設定 on the receiver: 受信機のモニター (GET / PUT /settings/output), the picture shown directly on
 * the monitor attached to the receiver. The usual settings flow: the choice is a draft (kept while
 * other screens are open, counted on the menu), the device checks it, and nothing is applied before
 * a confirmation with the change and what it does. Applying does not touch the transmission, so it
 * is allowed in every state. The operation screen's 低遅延表示 changes the same value at once (an
 * operation there, not a setting); the applied value here follows it.
 */
import { useEffect, useState } from 'react';
import { ApiFailure, api } from '../api/client';
import { useDrafts, useDraftState, useLive } from '../api/live';
import type { CheckResult, OutputDisplay, OutputSettings } from '../api/types';
import { Glyph, toneText } from '../components/Glyph';
import { Segmented } from '../components/Segmented';
import { Applied, ApplyDialog, DiscardDialog, FieldError, PendingPanel, Row, type PendingItem } from '../components/Settings';
import { Shell } from '../components/Shell';
import {
  displayName,
  fieldErrorText,
  monitorFailedHint,
  monitorRetrying,
  monitorState,
  outputHelp,
  outputNotDesigned,
  stateLabel,
} from '../i18n/ja';
import { timeOf } from '../lib/format';

const CHOICES: OutputDisplay[] = ['off', 'fullscreen', 'window'];

export function OutputSettingsPage() {
  const { device, status, stats, output, setOutput, clockOffsetMs } = useLive();
  const { setCount } = useDrafts();
  /** The chosen value, while it differs from the applied one. */
  const [stored, setStored] = useDraftState<OutputDisplay>('output', null);
  /** The device's answer for a draft (version and value). */
  const [check, setCheck] = useState<{ key: string; result: CheckResult } | null>(null);
  const [phase, setPhase] = useState<'edit' | 'confirm' | 'applying' | 'discard'>('edit');
  /** The applied settings the confirmation was opened with: it lists them, and applying carries their version. */
  const [base, setBase] = useState<OutputSettings | null>(null);
  const [doneAt, setDoneAt] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [recheck, setRecheck] = useState(0);

  const n = output && stored !== null && stored !== output.display ? 1 : 0;
  const key = output && stored !== null ? `${output.version}:${stored}` : '';
  useEffect(() => setCount('output', n), [n, setCount]);

  // The same value applied elsewhere (another screen, the operation screen's button): no draft left.
  useEffect(() => {
    if (output && stored !== null && stored === output.display) setStored(null);
  }, [output, stored, setStored]);

  // The device checks the draft 200 ms after it changed (and again when the confirmation opens).
  useEffect(() => {
    if (!output || stored === null || n === 0) {
      setCheck(null);
      return;
    }
    const input = { version: output.version, display: stored };
    const k = key;
    const t = setTimeout(() => {
      api.output
        .check(input)
        .then((result) => setCheck({ key: k, result }))
        .catch(() => undefined);
    }, 200);
    return () => clearTimeout(t);
    // `key` stands for the version and the draft
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, n, recheck]);

  // Applied elsewhere while the confirmation was open: what it lists is no longer what is applied.
  useEffect(() => {
    if (phase !== 'confirm' || !base || !output || output.version === base.version) return;
    setPhase('edit');
    setNotice('ほかの画面で受信機のモニターの設定が変わったため、確認を閉じました。確かめてからもう一度適用してください。');
  }, [phase, base, output]);

  if (!device || !status || !output) {
    return (
      <Shell route="video">
        <p className="muted">読み込み中…</p>
      </Shell>
    );
  }

  const statusLabel = stateLabel(device.role, status.state);
  const stopped = status.state === 'stopped';
  const draft = stored ?? output.display;
  const result = check?.key === key ? check.result : null;
  const err = result?.errors.find((e) => e.field === 'display');
  const impacts = result?.impacts ?? [];
  const item = (from: OutputDisplay): PendingItem[] =>
    n > 0 && stored !== null
      ? [{ key: 'display', label: '受信機のモニター', from: displayName(from), to: displayName(stored), revert: () => setStored(null) }]
      : [];
  const now = monitorState(stats?.display, output.display, stopped);

  const set = (d: OutputDisplay) => {
    setDoneAt(null);
    setNotice(null);
    setStored(d === output.display ? null : d);
  };

  const apply = async () => {
    if (!base || stored === null) return;
    setPhase('applying');
    try {
      const res = await api.output.apply({ version: base.version, display: stored });
      setOutput(res);
      setStored(null);
      setCheck(null);
      setDoneAt(timeOf(Date.now() + clockOffsetMs));
      setPhase('edit');
    } catch (e) {
      setPhase('edit');
      const code = e instanceof ApiFailure ? e.body?.error.code : undefined;
      if (code === 'version_conflict') {
        setNotice('ほかの画面で受信機のモニターの設定が変わったため、適用しませんでした。最新の値を読み込みました。');
        api.output.get().then(setOutput).catch(() => undefined);
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
            受信機 {device.id} は{statusLabel}です。変更は「適用」するまで機器に反映されません。
          </p>
        </div>
        <div className="set-body">
          <div className="set-form">
            <section aria-labelledby="sec-mon">
              <h2 id="sec-mon" className="sec-title first">
                受信機のモニター
              </h2>
              <Row label="映し方" labelId="lbl-display" hint={outputHelp} changed={n > 0} invalid={!!err} top>
                <Segmented
                  labelledBy="lbl-display"
                  value={draft}
                  onChange={set}
                  options={CHOICES.map((c) => ({ value: c, label: displayName(c) }))}
                />
                {err && <FieldError text={fieldErrorText(err)} />}
                {n > 0 && <Applied value={displayName(output.display)} onRevert={() => setStored(null)} />}
              </Row>
              <Row label="いまの様子" hint="受信機が知らせてくる様子です。1 秒ごとに更新します" top>
                <div className="measured">
                  <span className="monitor-state" style={{ color: toneText[now.tone], fontWeight: now.tone === 'warn' ? 600 : 400 }}>
                    <Glyph tone={now.tone} />
                    {now.text}
                  </span>
                </div>
                {now.tone === 'warn' && (
                  <p className="help">
                    {monitorFailedHint}
                    {monitorRetrying}
                  </p>
                )}
              </Row>
            </section>
            <div className="todo-note set-todo">
              <span style={{ marginTop: 4 }}>
                <Glyph tone="info" />
              </span>
              <span>{outputNotDesigned}</span>
            </div>
            {notice && (
              <p role="alert" style={{ margin: '16px 28px 0', fontWeight: 600, color: 'var(--err-text)' }}>
                {notice}
              </p>
            )}
          </div>

          <PendingPanel
            items={item(output.display)}
            impacts={impacts}
            invalid={!!err}
            stopped={stopped}
            onDiscard={() => n > 0 && setPhase('discard')}
            onApply={() => {
              if (n === 0 || err) return;
              setNotice(null);
              setBase(output);
              setRecheck((x) => x + 1);
              setPhase('confirm');
            }}
            doneAt={doneAt}
            doneNote="受信機のモニターの映し方を切り替えました。"
            emptyNote="未適用の変更はありません。表示中の値はすべて適用済みです。"
          />
        </div>
      </div>

      {(phase === 'confirm' || phase === 'applying') && n > 0 && base && (
        <ApplyDialog
          items={item(base.display)}
          impacts={impacts}
          stopped={stopped}
          context={`受信機 ${device.id} は${statusLabel}です。`}
          applying={phase === 'applying'}
          applyingText="適用しています…"
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
