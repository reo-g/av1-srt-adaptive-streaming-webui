import { useCallback, useEffect, useRef, useState } from 'react';
import { useLive } from '../api/live';
import { FullscreenIcon, Glyph } from '../components/Glyph';
import { useMonitorView } from '../components/Monitor';
import { hasPicture, mediaMeta, PreviewVideo, usePreviewActive } from '../components/Preview';
import { stateBadge, useDeviceNow } from '../components/Shell';
import { alertLabel, alertText, directFailed, directFsNote, directOn, monitorFailedHint, previewCaption } from '../i18n/ja';
import { timeOf } from '../lib/format';
import { closeFullscreen } from '../lib/fullscreen';

const HIDE_AFTER_MS = 3000;
const RECOVERED_FOR_MS = 10000;

/**
 * Full-screen picture (docs/screens.md「全画面表示」): the top band hides after 3 s
 * without input, warnings and errors never hide, nothing here changes settings. On the receiver
 * the preview stays closed while the picture goes to the receiver's own monitor (低遅延表示).
 */
export function FullscreenPage() {
  const { device, status } = useLive();
  const now = useDeviceNow();
  const monitor = useMonitorView();
  const active = usePreviewActive(status) && monitor === 'browser';
  const [band, setBand] = useState(true);
  const [safe, setSafe] = useState(false);
  const [recovered, setRecovered] = useState(false);
  const hideTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const hadAlerts = useRef(false);
  const exitBtn = useRef<HTMLButtonElement>(null);

  const wake = useCallback(() => {
    setBand(true);
    clearTimeout(hideTimer.current);
    hideTimer.current = setTimeout(() => setBand(false), HIDE_AFTER_MS);
  }, []);

  useEffect(() => {
    wake();
    exitBtn.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' || e.key.toLowerCase() === 'f') {
        e.preventDefault();
        closeFullscreen();
        return;
      }
      wake();
    };
    // Leaving browser full screen (Esc is handled by the browser) closes this view too.
    const onFs = () => {
      if (!document.fullscreenElement) closeFullscreen();
    };
    addEventListener('keydown', onKey);
    addEventListener('mousemove', wake);
    addEventListener('touchstart', wake, { passive: true });
    document.addEventListener('fullscreenchange', onFs);
    return () => {
      clearTimeout(hideTimer.current);
      removeEventListener('keydown', onKey);
      removeEventListener('mousemove', wake);
      removeEventListener('touchstart', wake);
      document.removeEventListener('fullscreenchange', onFs);
    };
  }, [wake]);

  // Show「回復しました」for 10 s after the last alert clears.
  const alertCount = status?.alerts.length ?? 0;
  useEffect(() => {
    if (alertCount > 0) {
      hadAlerts.current = true;
      setRecovered(false);
      return;
    }
    if (!hadAlerts.current) return;
    hadAlerts.current = false;
    setRecovered(true);
    const t = setTimeout(() => setRecovered(false), RECOVERED_FOR_MS);
    return () => clearTimeout(t);
  }, [alertCount]);

  if (!device || !status) return <div className="fs" />;
  const tx = device.role === 'tx';
  const badge = stateBadge(device.role, status, now);
  const worst = status.alerts.some((a) => a.level === 'error') ? 'error' : 'warn';
  const lead = status.alerts.find((a) => a.level === worst);
  const picture = hasPicture(status.media);
  const live = status.state === 'live';
  const reason = tx
    ? '入力信号を検出できません'
    : status.state === 'stopped'
      ? '受信を停止しています'
      : status.state === 'live'
        ? '映像を待っています'
        : '送信機からの接続を待っています';

  return (
    <div className="fs" style={{ cursor: band ? 'default' : 'none' }}>
      <h1 className="sr-only">{tx ? '入力映像' : '受信映像'}（全画面）</h1>
      <div className="fs-stage" onClick={() => (band ? setBand(false) : wake())}>
        <div className="fs-frame">
          {active ? (
            <PreviewVideo role={device.role} label={tx ? '入力映像' : '受信映像'} />
          ) : monitor === 'direct' || monitor === 'failed' ? (
            <div className="ph none" role="status">
              {monitor === 'direct' ? directOn : directFailed}
              <small>{monitor === 'direct' ? directFsNote : monitorFailedHint}</small>
            </div>
          ) : (
            <div className={picture ? 'ph' : 'ph none'}>
              {picture ? (tx ? '入力映像' : '受信映像') : tx ? '入力映像なし' : '受信映像なし'}
              {!picture ? <small>{reason}</small> : !live && <small>{previewCaption(device.role, false)}</small>}
            </div>
          )}
          {safe && (
            <>
              <div aria-hidden="true" className="safe" style={{ inset: '3.5%' }}>
                <span>アクション 93%</span>
              </div>
              <div aria-hidden="true" className="safe" style={{ inset: '5%', borderColor: 'rgba(255,255,255,0.32)' }}>
                <span style={{ color: 'rgba(255,255,255,0.6)' }}>タイトル 90%</span>
              </div>
            </>
          )}
        </div>
      </div>

      <div className="fs-top">
        {lead ? (
          <section role="status" aria-label="現在の状況" className={`fs-strip tone-${worst === 'error' ? 'err' : 'warn'}`}>
            <span className="strip-label">
              <Glyph tone={worst === 'error' ? 'err' : 'warn'} size={12} />
              {alertLabel(lead)}
            </span>
            <span className="strip-text">
              {status.alerts
                .filter((a) => a.level === worst || worst === 'warn')
                .map((a) => alertText(device.role, a, status.peer.id))
                .join('')}
            </span>
          </section>
        ) : recovered ? (
          <section role="status" aria-label="現在の状況" className="fs-strip tone-ok">
            <span className="strip-label">
              <Glyph tone="ok" size={12} />
              回復しました
            </span>
            <span className="strip-text">{timeOf(now)} に正常に戻りました。</span>
          </section>
        ) : null}

        <div className={band ? 'fs-band' : 'fs-band hidden'} aria-hidden={!band}>
          <div className="left">
            <span style={{ fontSize: 18, fontWeight: 600 }}>{device.id}</span>
            <span>{tx ? '入力映像' : '受信映像'}</span>
            <span className="dim">{mediaMeta(status.media)}</span>
          </div>
          <div className="right">
            <span style={{ display: 'inline-flex', alignItems: 'center', gap: 8 }}>
              <Glyph tone={badge.tone} dark />
              <span style={{ fontWeight: 600, color: badge.tone === 'ok' ? '#7fd9aa' : badge.tone === 'warn' ? '#f5c26b' : '#c3cad3' }}>
                {badge.label}
              </span>
              <span className="dim">{badge.sub}</span>
            </span>
            <span aria-hidden="true" style={{ width: 1, height: 18, background: 'rgba(255,255,255,0.25)' }} />
            <span style={{ fontSize: 18, fontWeight: 500 }}>
              {timeOf(now)}
              <span style={{ marginLeft: 4, fontSize: 14, fontWeight: 400, color: '#c3cad3' }}>JST</span>
            </span>
            <button type="button" className="fs-btn" aria-pressed={safe} onClick={() => setSafe(!safe)}>
              セーフエリア
            </button>
            <button ref={exitBtn} type="button" className="fs-btn" onClick={closeFullscreen}>
              <FullscreenIcon exit />
              全画面を終了<kbd>Esc</kbd>
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
