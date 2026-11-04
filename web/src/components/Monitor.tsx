/**
 * 受信機のモニター on the receiver's operation screen (docs/screens.md「運用」). The received picture
 * goes either to the browser preview or, with 低遅延表示, straight to the monitor attached to the
 * receiver (OutputSettings.display, POST /display: an operation, no draft). While the receiver shows
 * it there, and until the applied value is known, the browser preview stays closed: no second
 * decode on the receiver, no extra delay.
 */
import { useEffect, useState } from 'react';
import { api } from '../api/client';
import { useLive } from '../api/live';
import type { OutputDisplay, OutputSettings, Stats, Status } from '../api/types';
import {
  directBack,
  directButton,
  directCaption,
  directCoversNote,
  directFailed,
  directMode,
  directOffer,
  directOn,
  directOnStopped,
  directProblem,
  directToFullscreen,
  directToWindow,
  monitorFailedHint,
  monitorRetrying,
  monitorState,
} from '../i18n/ja';
import { Glyph, toneText } from './Glyph';
import { OpsPreview } from './Preview';

/**
 * browser  the browser preview, as before (with the 低遅延表示 button)
 * waiting  the applied value is not known yet: nothing is opened
 * direct   the picture goes to the receiver's monitor (or will, once receiving starts)
 * failed   the receiver could not show it there
 */
export type MonitorView = 'browser' | 'waiting' | 'direct' | 'failed';

/**
 * Direct while the receiver says it shows the picture, or while the applied value is on and it has
 * not said otherwise yet; failed only while the value is on (after 「ブラウザ表示に戻す」 the old report
 * no longer counts).
 */
export function monitorView(output: OutputSettings | null, known: boolean, reported: Stats['display'] | undefined): MonitorView {
  if (!known) return 'waiting';
  const on = !!output && output.display !== 'off';
  if (on && reported === 'failed') return 'failed';
  if (reported === 'on' || on) return 'direct';
  return 'browser';
}

/** The view on this device (the transmitter always has the browser preview). */
export function useMonitorView(): MonitorView {
  const { device, output, outputKnown, stats } = useLive();
  if (!device || device.role !== 'rx') return 'browser';
  return monitorView(output, outputKnown, stats?.display);
}

/** The operation screen's picture box on the receiver, and what goes with it. */
export function RxPicture({ status }: { status: Status }) {
  const { output, outputKnown, stats, setOutput } = useLive();
  /** The mode being asked for (POST /display on its way). */
  const [asking, setAsking] = useState<OutputDisplay | null>(null);
  const [problem, setProblem] = useState(false);
  const stopped = status.state === 'stopped';
  let view = monitorView(output, outputKnown, stats?.display);
  // 低遅延表示 was pressed: the browser preview closes at once, before the device is asked.
  if (asking !== null && asking !== 'off' && view === 'browser') view = 'direct';

  // After the render that closed the preview (its session is closed in its cleanup).
  useEffect(() => {
    if (asking === null) return;
    let gone = false;
    api
      .display(asking)
      .then(setOutput)
      .catch(() => !gone && setProblem(true))
      .finally(() => !gone && setAsking(null));
    return () => {
      gone = true;
    };
  }, [asking, setOutput]);

  const ask = (mode: OutputDisplay) => {
    if (asking !== null) return;
    setProblem(false);
    setAsking(mode);
  };
  const busy = asking !== null;
  const notice = problem && (
    <p role="alert" className="direct-problem">
      <Glyph tone="err" />
      <span>{directProblem}</span>
    </p>
  );

  if (view === 'browser') {
    return (
      <>
        <OpsPreview status={status} role="rx" />
        <div className="direct-offer">
          <button type="button" className="btn btn-sm" disabled={busy} onClick={() => ask('window')}>
            {directButton}
          </button>
          <p>{directOffer}</p>
        </div>
        {notice}
      </>
    );
  }

  const mode = asking ?? output?.display ?? 'off';
  return (
    <>
      <div className="preview">
        {view === 'waiting' ? (
          <span>受信映像</span>
        ) : (
          <div className="pv-overlay">
            <div className="pv-status" role="status">
              {view === 'failed' ? (
                <>
                  <span className="pv-lead">
                    <Glyph tone="warn" dark />
                    {directFailed}
                  </span>
                  <small>
                    {monitorFailedHint}
                    {monitorRetrying}
                  </small>
                </>
              ) : mode === 'off' ? (
                // 「ブラウザ表示に戻す」: the receiver still shows it until it has closed its window
                <span className="pv-lead">ブラウザ表示に戻しています…</span>
              ) : (
                <>
                  <span className="pv-lead">
                    <Glyph tone={stopped ? 'off' : 'ok'} dark />
                    {stopped ? directOnStopped : directOn}
                  </span>
                  <small>{directMode(mode, stopped)}</small>
                </>
              )}
            </div>
            {mode !== 'off' && (
              <div className="pv-actions">
                <button type="button" className="btn btn-sm" disabled={busy} onClick={() => ask('off')}>
                  {directBack}
                </button>
                {view === 'direct' && (
                  <button type="button" className="btn btn-sm" disabled={busy} onClick={() => ask(mode === 'fullscreen' ? 'window' : 'fullscreen')}>
                    {mode === 'fullscreen' ? directToWindow : directToFullscreen}
                  </button>
                )}
              </div>
            )}
            {view === 'direct' && mode === 'window' && <small>{directCoversNote}</small>}
          </div>
        )}
      </div>
      {view !== 'waiting' && <p className="pv-caption">{directCaption}</p>}
      {notice}
    </>
  );
}

/** 「受信機のモニター」 under the picture: what the receiver reports (shape and words). */
export function MonitorLine({ status, view }: { status: Status; view: MonitorView }) {
  const { output, stats } = useLive();
  const st = monitorState(stats?.display, output?.display ?? null, status.state === 'stopped');
  return (
    <div className="audio-line">
      <span>受信機のモニター</span>
      <span className="monitor-state" style={{ color: toneText[st.tone], fontWeight: st.tone === 'warn' ? 600 : 400 }}>
        <Glyph tone={st.tone} />
        {st.text}
      </span>
      {/* the picture box says it when the value is on */}
      {st.tone === 'warn' && view !== 'failed' && <span className="muted">{monitorFailedHint}</span>}
    </div>
  );
}
