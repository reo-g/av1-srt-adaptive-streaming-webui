import { useCallback, useEffect, useRef, useState, type RefObject } from 'react';
import { ApiFailure, api } from '../api/client';
import type { Media, Role, Status } from '../api/types';
import {
  previewAutoRetry,
  previewCaption,
  previewConnecting,
  previewProblemHint,
  previewProblemText,
  previewRetry,
  type PreviewProblem,
} from '../i18n/ja';

/** The preview is closed while the tab is in the background (device load). */
export function usePageVisible(): boolean {
  const [visible, setVisible] = useState(() => document.visibilityState === 'visible');
  useEffect(() => {
    const on = () => setVisible(document.visibilityState === 'visible');
    document.addEventListener('visibilitychange', on);
    return () => document.removeEventListener('visibilitychange', on);
  }, []);
  return visible;
}

/** "カメラ Facecam 4K　3840×2160　30p". */
export const mediaMeta = (m: Media): string => {
  if (m.kind === 'output') {
    if (m.black || !m.format) return `${m.label}　黒を出力中`;
    return `${m.label}　${m.format}`;
  }
  const kind = m.kind === 'camera' ? 'カメラ' : '映像ファイル';
  return m.format ? `${kind} ${m.label}　${m.format}` : `${kind} ${m.label}　信号なし`;
};

/** Whether there is a picture to show: tx shows its input, rx shows what it outputs. */
export const hasPicture = (m: Media) => !!m.format && !m.black;

// ---------------------------------------------------------------- session

/**
 * The device answers a receive-only offer (POST /preview/sessions) and streams the picture
 * to the browser: tx what it sends (after compression), rx what it shows. A session lives
 * as long as a PreviewVideo is mounted; the parents mount it only while the link is live,
 * there is a picture and the tab is visible.
 */
type View =
  | { phase: 'connecting' }
  | { phase: 'playing' }
  /** auto: another attempt is scheduled (after a session dropped). */
  | { phase: 'problem'; problem: PreviewProblem; auto: boolean };

const BACKOFF_S = [3, 6, 12, 30];
const ICE_WAIT_MS = 2000;
/** "disconnected" can recover by itself; give up on it after this long. */
const DISCONNECTED_MS = 5000;
/** From the answer to the first picture. The device drops a viewer that is not connected after 15 s. */
const FIRST_PICTURE_MS = 15000;

/** Resolves when candidate gathering is complete, or after `ms` with what was gathered by then. */
function gathered(pc: RTCPeerConnection, ms: number): Promise<void> {
  return new Promise((resolve) => {
    if (pc.iceGatheringState === 'complete') return resolve();
    const done = () => {
      clearTimeout(timer);
      pc.removeEventListener('icegatheringstatechange', check);
      resolve();
    };
    const check = () => {
      if (pc.iceGatheringState === 'complete') done();
    };
    const timer = setTimeout(done, ms);
    pc.addEventListener('icegatheringstatechange', check);
  });
}

/**
 * Ask for the device's preferred formats first (the newer one, then the widely supported
 * one); everything else, retransmission and repair entries included, keeps the browser's order.
 */
function preferFormats(tr: RTCRtpTransceiver) {
  if (typeof RTCRtpReceiver === 'undefined' || typeof RTCRtpReceiver.getCapabilities !== 'function') return;
  if (typeof tr.setCodecPreferences !== 'function') return;
  const caps = RTCRtpReceiver.getCapabilities('video');
  if (!caps) return;
  const rank = (mime: string) => {
    const m = mime.toLowerCase();
    return m === 'video/h265' ? 0 : m === 'video/h264' ? 1 : 2;
  };
  const codecs = caps.codecs
    .map((c, i) => ({ c, i }))
    .sort((a, b) => rank(a.c.mimeType) - rank(b.c.mimeType) || a.i - b.i)
    .map((x) => x.c);
  try {
    tr.setCodecPreferences(codecs);
  } catch {
    /* keep the browser's order */
  }
}

function usePreviewSession(video: RefObject<HTMLVideoElement | null>) {
  const [view, setView] = useState<View>({ phase: 'connecting' });
  const [attempt, setAttempt] = useState(0);
  /** Index of the next automatic retry delay; > 0 while reconnecting after a drop. */
  const backoff = useRef(0);
  const autoTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  const retry = useCallback(() => {
    clearTimeout(autoTimer.current);
    backoff.current = 0;
    setAttempt((a) => a + 1);
  }, []);

  // Coming back from the browser's page cache: the old session is gone.
  useEffect(() => {
    const on = (e: PageTransitionEvent) => {
      if (e.persisted) retry();
    };
    addEventListener('pageshow', on);
    return () => removeEventListener('pageshow', on);
  }, [retry]);

  useEffect(() => {
    if (typeof RTCPeerConnection === 'undefined') {
      setView({ phase: 'problem', problem: 'unsupported', auto: false });
      return;
    }
    let pc: RTCPeerConnection;
    try {
      pc = new RTCPeerConnection({ iceServers: [] });
    } catch {
      // Present but turned off (browser policy or settings). Throwing here would take the whole screen down.
      setView({ phase: 'problem', problem: 'unsupported', auto: false });
      return;
    }
    setView({ phase: 'connecting' });
    const v = video.current;
    let over = false; // this attempt has ended (closed, failed or replaced)
    let shown = false; // the first picture has been shown
    let id: string | null = null;
    let lostTimer: ReturnType<typeof setTimeout> | undefined;
    let firstTimer: ReturnType<typeof setTimeout> | undefined;

    const release = () => {
      clearTimeout(lostTimer);
      clearTimeout(firstTimer);
      pc.ontrack = null;
      pc.onconnectionstatechange = null;
      pc.oniceconnectionstatechange = null;
      v?.removeEventListener('playing', onPlaying);
      removeEventListener('pagehide', onHide);
      pc.close();
      if (v) v.srcObject = null;
      if (id) api.stopPreview(id).catch(() => undefined);
      id = null;
    };
    const end = () => {
      if (over) return false;
      over = true;
      release();
      return true;
    };
    /** auto: try again by itself, waiting 3 → 6 → 12 → 30 s. */
    const fail = (problem: PreviewProblem, auto: boolean) => {
      if (!end()) return;
      if (auto) {
        const s = BACKOFF_S[Math.min(backoff.current, BACKOFF_S.length - 1)];
        backoff.current += 1;
        autoTimer.current = setTimeout(() => setAttempt((a) => a + 1), s * 1000);
      }
      setView({ phase: 'problem', problem, auto });
    };
    function onPlaying() {
      if (over) return;
      shown = true;
      clearTimeout(firstTimer);
      backoff.current = 0;
      setView({ phase: 'playing' });
    }
    // Cleanup does not run when the page is closed: close the session here so the device frees it.
    function onHide() {
      if (end()) setView({ phase: 'connecting' });
    }
    v?.addEventListener('playing', onPlaying);
    addEventListener('pagehide', onHide);

    pc.ontrack = (e) => {
      if (!v || over) return;
      v.srcObject = e.streams[0] ?? new MediaStream([e.track]);
      v.play().catch(() => undefined);
    };
    pc.onconnectionstatechange = pc.oniceconnectionstatechange = () => {
      const states = [pc.connectionState, pc.iceConnectionState];
      if (states.includes('failed')) return fail('failed', true);
      if (states.includes('disconnected')) {
        lostTimer ??= setTimeout(() => fail('failed', true), DISCONNECTED_MS);
        return;
      }
      clearTimeout(lostTimer);
      lostTimer = undefined;
    };

    (async () => {
      try {
        preferFormats(pc.addTransceiver('video', { direction: 'recvonly' }));
        await pc.setLocalDescription(await pc.createOffer());
        await gathered(pc, ICE_WAIT_MS);
        const offer = pc.localDescription?.sdp;
        if (over) return;
        if (!offer) return fail('failed', false);
        let session;
        try {
          session = await api.startPreview(offer);
        } catch (err) {
          const code = err instanceof ApiFailure ? err.body?.error.code : undefined;
          if (code === 'too_many_viewers') return fail('busy', false);
          if (code === 'no_common_codec') return fail('unsupported', false);
          // The link has just gone down (the screen follows in a moment) or the device is
          // still restarting the preview after a drop: keep trying in the background.
          return fail('failed', code === 'not_live' || backoff.current > 0);
        }
        if (over) {
          api.stopPreview(session.id).catch(() => undefined);
          return;
        }
        id = session.id;
        await pc.setRemoteDescription({ type: 'answer', sdp: session.answer });
        if (!shown) firstTimer = setTimeout(() => fail('failed', true), FIRST_PICTURE_MS);
      } catch {
        fail('failed', backoff.current > 0);
      }
    })();

    return () => {
      clearTimeout(autoTimer.current);
      end();
    };
  }, [attempt, video]);

  return { view, retry };
}

// ---------------------------------------------------------------- views

/**
 * The live picture with its overlays (preparing, cannot show + 「もう一度試す」).
 * Placed inside a positioned 16:9 box (.preview or .fs-frame).
 */
export function PreviewVideo({ role, label }: { role: Role; label: string }) {
  const video = useRef<HTMLVideoElement>(null);
  const { view, retry } = usePreviewSession(video);
  // The status region stays mounted (empty while playing) so that screen readers announce a
  // later 「表示できませんでした」; a region inserted together with its text is often not read.
  // The button is outside it, so its name is not read out with every change.
  return (
    <>
      <video ref={video} className="pv-video" autoPlay muted playsInline aria-label={label} />
      <div className={view.phase === 'playing' ? 'pv-overlay idle' : 'pv-overlay'}>
        <div className="pv-status" role="status">
          {view.phase === 'connecting' && <span>{previewConnecting}</span>}
          {view.phase === 'problem' && (
            <>
              <span className="pv-problem">{previewProblemText(view.problem)}</span>
              <small>
                {previewProblemHint(role, view.problem)}
                {view.auto && previewAutoRetry}
              </small>
            </>
          )}
        </div>
        {view.phase === 'problem' && view.problem !== 'unsupported' && (
          <button
            type="button"
            className="btn btn-sm"
            onClick={(e) => {
              e.stopPropagation(); // full screen: a click on the picture toggles the band
              retry();
            }}
          >
            {previewRetry}
          </button>
        )}
      </div>
    </>
  );
}

/** Whether a preview session should be open now: live, a picture, and the tab in front. */
export function usePreviewActive(status: Status | null): boolean {
  const visible = usePageVisible();
  return !!status && status.state === 'live' && hasPicture(status.media) && visible;
}

/** The operation screen's preview box and the caption under it. */
export function OpsPreview({ status, role }: { status: Status; role: Role }) {
  const active = usePreviewActive(status);
  const picture = hasPicture(status.media);
  const live = status.state === 'live';
  const placeholder = role === 'tx' ? '入力映像' : picture ? '受信映像' : '受信映像なし';
  const label = role === 'tx' ? '入力映像のプレビュー' : '受信映像のプレビュー';
  return (
    <>
      <div className={`preview${picture ? '' : ' empty'}`}>
        {active ? <PreviewVideo role={role} label={label} /> : <span>{placeholder}</span>}
      </div>
      {(picture || !live) && <p className="pv-caption">{previewCaption(role, live)}</p>}
    </>
  );
}
