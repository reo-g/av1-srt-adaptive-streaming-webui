/**
 * Every user-facing sentence that depends on API codes lives here.
 * Wording follows docs/terms.md (用語の言い換え対応表): do not introduce
 * implementation terms. The API sends codes and numbers; the UI owns the words.
 * The people using the devices are not engineers: say what is happening and
 * what to do, in plain words.
 */
import type {
  Alert,
  BitrateReason,
  Cipher,
  DelayProfile,
  DeviceEvent,
  FieldError,
  Impact,
  LinkState,
  NetworkTestDirection,
  NetworkTestError,
  NetworkTestMode,
  ReachabilityTarget,
  Role,
  TransportMode,
  VideoEncoding,
} from '../api/types';

const n1 = (v: number | string | undefined) => (typeof v === 'number' ? v.toFixed(1) : String(v ?? ''));
const peerOf = (role: Role, id: string | null | undefined) =>
  role === 'tx' ? `受信機 ${id ?? ''}`.trim() : `送信機 ${id ?? ''}`.trim();

export const roleName = (role: Role) => (role === 'tx' ? '送信機' : '受信機');

export const stateLabel = (role: Role, s: LinkState): string => {
  switch (s) {
    case 'live':
      return role === 'tx' ? '伝送中' : '受信中';
    case 'connecting':
      return '接続中';
    case 'reconnecting':
      return '再接続中';
    case 'listening':
      return '待ち受け中';
    case 'stopped':
      return '停止中';
  }
};

export const alertLabel = (a: Alert): string => {
  if (a.level === 'warn') return '警告';
  if (a.code === 'peer_rejected_passphrase' || a.code === 'peer_unreachable') return '接続エラー';
  if (a.code === 'input_lost') return '入力エラー';
  return 'エラー';
};

/** "5 分" from a window in seconds. */
const windowText = (s: number | string | undefined) => {
  const v = Number(s ?? 300);
  return v % 60 === 0 ? `${v / 60} 分` : `${v} 秒`;
};

export const alertText = (role: Role, a: Alert, peerId: string | null): string => {
  const p = a.params ?? {};
  switch (a.code) {
    case 'delay_margin_low':
      return (
        `回線が混み合い、映像が止まるまでの余裕が ${n1(p.margin_s)} 秒になっています。このまま続くと受信側で映像が止まります。` +
        'おまかせでは自動で画質を下げます。画質を固定しているときは、映像設定で画質を下げてください。'
      );
    case 'video_stalled':
      return `直近 ${windowText(p.window_s)}で映像が ${p.count} 回止まりました（合計 ${n1(p.seconds)} 秒）。続くときは、送信機の映像設定で画質を下げてください。`;
    case 'loss_high':
      return `回線の損失が多く、映像が乱れるおそれがあります（${n1(p.loss_pct)}%）。`;
    case 'peer_rejected_passphrase':
      return role === 'tx'
        ? `${peerOf(role, peerId)}が接続を拒否しています。パスフレーズが一致していません。`
        : `${peerOf(role, peerId)}からの接続を拒否しました。パスフレーズが一致していません。`;
    case 'peer_unreachable':
      return `${peerOf(role, peerId)}に接続できません。ケーブルと${role === 'tx' ? '受信機' : '送信機'}の電源を確認してください。`;
    case 'input_lost':
      return '入力信号を検出できません。カメラの接続を確認してください。';
  }
};

const bitrateText = (p: DeviceEvent['params']): string => {
  const q = p ?? {};
  const to = n1(q.mbps);
  const step = q.from_mbps !== undefined ? `${n1(q.from_mbps)} → ${to}` : to;
  switch (q.reason as BitrateReason | undefined) {
    case 'loss':
      return `回線の混雑で損失が増えたため、画質を ${step} Mbps に下げました`;
    case 'delay':
      return `送りきれない映像が送信機に溜まってきたため、画質を ${step} Mbps に下げました`;
    case 'recovered':
      return `回線に余裕が戻ったため、画質を ${step} Mbps に上げました`;
    default:
      return `画質を ${step} Mbps に変更しました`;
  }
};

export const eventText = (role: Role, e: DeviceEvent, peerId: string | null): string => {
  const p = e.params ?? {};
  switch (e.code) {
    case 'delay_margin_low':
      return `回線が混み合い、映像が止まるまでの余裕が ${n1(p.margin_s)} 秒まで減りました`;
    case 'video_stalled':
      return `映像が ${n1(p.seconds)} 秒止まりました`;
    case 'loss_high':
      return `回線の損失が多くなりました（${n1(p.loss_pct)}%）`;
    case 'peer_rejected_passphrase':
      return role === 'tx'
        ? `${peerOf(role, peerId)}が接続を拒否しました（パスフレーズの不一致）`
        : `${peerOf(role, peerId)}からの接続を拒否しました（パスフレーズの不一致）`;
    case 'peer_disconnected':
      return `${peerOf(role, peerId)}との接続が切れました`;
    case 'peer_connected':
      return role === 'tx' ? `${peerOf(role, peerId)}に接続しました` : `${peerOf(role, peerId)}から接続されました`;
    case 'transport_started':
      return role === 'tx' ? '伝送を開始しました' : `受信を開始しました（${p.format ?? ''}）`;
    case 'transport_stopped':
      return role === 'tx' ? '伝送を停止しました' : '受信を停止しました';
    case 'listening_started':
      return `ポート ${p.port} で待ち受けを開始しました`;
    case 'input_detected':
      return `入力信号を検出しました（${p.format}）`;
    case 'device_started':
      return '機器を起動しました';
    case 'bitrate_changed':
      return bitrateText(p);
    case 'video_settings_applied':
      return '映像設定を適用しました';
    case 'transport_settings_applied':
      return '伝送設定を適用しました';
    case 'passphrase_changed':
      return 'パスフレーズを変更しました';
    case 'network_test_done': {
      const d = p.direction === 'to_peer' || p.direction === 'from_peer' ? `${directionText(role, p.direction)}、` : '';
      return `回線テストが終わりました（${d}${n1(p.mbps)} Mbps）`;
    }
  }
};

/** "約 10 秒" from a gap in seconds, "しばらく" when the device did not say. */
const gapText = (s: number | string | undefined) => (s === undefined ? 'しばらく' : `約 ${s} 秒`);

/**
 * The impacts as they are listed. A new input while live comes with reconnect_gap; both
 * describe the same gap, so only the input line (which names both causes) is shown.
 */
export const visibleImpacts = (list: Impact[]): Impact[] =>
  list.some((i) => i.code === 'input_switch_gap') ? list.filter((i) => i.code !== 'reconnect_gap') : list;

export const impactText = (i: Impact): string => {
  const p = i.params ?? {};
  switch (i.code) {
    case 'input_switch_gap':
      return `入力を切り替えて接続をやり直すため、受信側の映像が${gapText(p.gap_s)}途切れます`;
    case 'switch_no_gap':
      return '映像は途切れず、数秒で新しい画質に切り替わります。';
    case 'manual_fixed':
      return '画質を固定すると、回線が混んでも自動では下げません';
    case 'auto_decides':
      return '画質（ビットレート）は回線に合わせて自動で調整します。解像度とフレームレートは入力のままです。';
    case 'bitrate_lower_quality':
      return '画質を下げると、動きの多い場面で映像が粗くなることがあります';
    case 'bitrate_near_bandwidth':
      return `回線の推定容量は ${n1(p.bandwidth_mbps)} Mbps です。固定する画質がこれに近いか上回っているため、映像が止まるおそれがあります。画質を下げるか「おまかせ」を選んでください`;
    case 'reconnect':
      return '接続をやり直します';
    case 'reconnect_gap':
      return `接続をやり直すため、受信側の映像が${gapText(p.gap_s)}途切れます`;
    case 'no_peer_effect':
      return 'いまは未接続のため、受信側への追加の影響はありません';
    case 'saved_while_stopped':
      return '設定を保存します。伝送は停止したままです';
  }
};

export const fieldErrorText = (e: FieldError): string => {
  const p = e.params ?? {};
  switch (e.field) {
    case 'encode.manual.bitrate_mbps':
      return `${n1(p.min)}〜${n1(p.max)} の数値を入力してください`;
    case 'encryption.passphrase':
      // The allowed length is shown only when the value is out of range (docs/terms.md).
      return `${p.min}〜${p.max} 文字で入力してください`;
    case 'peer.host':
      return 'IP アドレスの形式で入力してください';
    case 'peer.port':
    case 'listen_port':
      return '1〜65535 の数値を入力してください';
    case 'delay.profile':
      return 'この遅延はまだ選べません';
    case 'input.loop':
      return '繰り返し再生はまだ使えません';
    case 'rate_mbps':
      return `送る速さは ${p.min}〜${p.max} Mbps で選んでください`;
    case 'duration_s':
      return `時間は ${p.min}〜${p.max} 秒で選んでください`;
    default:
      return '入力内容を確認してください';
  }
};

export const modeName = (m: TransportMode) => ({ outbound: '送信機から接続', inbound: '接続を待つ', mutual: '互いに接続' })[m];
/** Shown instead of the control when only one connection method exists. */
export const modeSentence = (m: TransportMode) =>
  ({
    outbound: 'この送信機から受信機へ接続します',
    inbound: '受信機からの接続を待ちます',
    mutual: '送信機と受信機が互いに接続します',
  })[m];
export const cipherName = (c: Cipher) =>
  ({ 'aes-128': 'AES-128', 'aes-192': 'AES-192', 'aes-256': 'AES-256', none: 'なし' })[c];

// ---------------------------------------------------------------- video quality

export const videoModeName = (m: 'auto' | 'manual') => (m === 'auto' ? 'おまかせ（おすすめ）' : '画質を固定');
export const videoModeShort = (m: 'auto' | 'manual') => (m === 'auto' ? 'おまかせ' : '固定');
export const videoModeHelp = (m: 'auto' | 'manual') =>
  m === 'auto'
    ? '回線が混んだら画質を下げ、空いたら戻します。映像が止まりにくくなります。'
    : '選んだ画質のまま送ります。回線が混むと映像が遅れたり止まったりすることがあります。';

/** "AV1・入力と同じ解像度とフレームレート（3840×2160・30p）" */
export const encodingText = (e: VideoEncoding, signal: string | null): string => {
  const codec = { av1: 'AV1' }[e.codec];
  const [res, fps] = (signal ?? '').split('　');
  const input = res && fps ? `（${res}・${fps}）` : '';
  return `${codec}・入力と同じ解像度とフレームレート${input}`;
};

// ---------------------------------------------------------------- delay

/** "約 10 秒" from 9500 ms, "約 1.5 秒" from 1500 ms. */
export const aboutSeconds = (ms: number) => {
  const s = ms / 1000;
  return `約 ${s >= 5 ? Math.round(s) : Number(s.toFixed(1))} 秒`;
};
export const delayName = (p: DelayProfile, targetMs?: number) => {
  const name = p === 'standard' ? '標準' : '低遅延';
  return targetMs ? `${name}（${aboutSeconds(targetMs)}）` : name;
};
export const delayDesc = (p: DelayProfile) =>
  p === 'standard' ? '回線が乱れても映像が止まりにくい' : 'おまかせは使えず、画質は固定になります。回線が乱れると映像が止まりやすい';

// ---------------------------------------------------------------- preview

/** Why the preview cannot be shown (from the device's error code or the browser's abilities). */
export type PreviewProblem = 'failed' | 'busy' | 'unsupported';

export const previewConnecting = 'プレビューを準備しています';
export const previewRetry = 'もう一度試す';
export const previewAutoRetry = '少し待つと、自動でもう一度つなぎます。';

export const previewProblemText = (p: PreviewProblem): string =>
  ({
    failed: 'プレビューを表示できませんでした',
    busy: 'ほかの画面でプレビューを見ているため、表示できません',
    unsupported: 'このブラウザではプレビューを表示できません',
  })[p];

/** The line under the problem: what it means for the video itself, or what to do. */
export const previewProblemHint = (role: Role, p: PreviewProblem): string => {
  switch (p) {
    case 'busy':
      return 'ほかの画面を閉じてから、もう一度試してください。';
    case 'unsupported':
      return 'Google Chrome か Safari で開いてください。';
    case 'failed':
      return role === 'tx' ? '受信機へ送っている映像には影響ありません。' : '受信機が出力している映像には影響ありません。';
  }
};

/**
 * Caption under the preview. tx shows the picture after compression (what the receiver
 * gets), a few seconds behind the input; rx shows its own output, slightly behind it.
 */
export const previewCaption = (role: Role, live: boolean): string => {
  if (!live) return role === 'tx' ? 'プレビューは伝送中だけ表示します。' : 'プレビューは受信中だけ表示します。';
  return role === 'tx'
    ? '受信機へ送っている映像を、圧縮したあとの画質で確認用に表示しています。入力より数秒遅れて映ります。'
    : '受信機が出力している映像を、確認用に表示しています。出力より少し遅れて映ります。';
};

// ---------------------------------------------------------------- network test

const peerName = (role: Role) => (role === 'tx' ? '受信機' : '送信機');

/** "送信機 → 受信機": a direction seen from this device. */
export const directionText = (role: Role, d: NetworkTestDirection) =>
  d === 'to_peer' ? `${roleName(role)} → ${peerName(role)}` : `${peerName(role)} → ${roleName(role)}`;

/** The choice of direction; on the transmitter the video's direction is marked. */
export const directionChoice = (role: Role, d: NetworkTestDirection) =>
  role === 'tx' && d === 'to_peer' ? `${directionText(role, d)}（映像の向き）` : directionText(role, d);

export const testModeName = (m: NetworkTestMode) =>
  m === 'max' ? 'いちばん速く送れる速さを測る' : '決めた速さで送って、損失と揺らぎを見る';

/** Short form for the list of results. */
export const testModeShort = (m: NetworkTestMode, rateMbps: number | null) =>
  m === 'max' ? 'いちばん速く送れる速さ' : `${rateMbps ?? ''} Mbps で送る`.trim();

/** ● 届く ▲ 一部届かない ■ 届かない (with the shape, never by colour alone). */
export type ReachLevel = 'ok' | 'partial' | 'none';
export const reachLevel = (t: ReachabilityTarget): ReachLevel =>
  t.sent > 0 && t.received >= t.sent ? 'ok' : t.received > 0 ? 'partial' : 'none';
export const reachLabel = (l: ReachLevel) => ({ ok: '届く', partial: '一部届かない', none: '届かない' })[l];

/** 受信機 / 送信機 for the peer, the configured name for a device on the way. */
export const targetName = (role: Role, t: ReachabilityTarget) => (t.kind === 'peer' ? peerName(role) : (t.label ?? t.address));

export const reachNoPeer = (role: Role) =>
  role === 'rx'
    ? '送信機のアドレスがわかりません。一度伝送すると確かめられます。'
    : '受信機のアドレスがわかりません。伝送設定で接続先を入力してください。';
export const reachFailed = '確かめられませんでした。もう一度お試しください。';

/** A test that ended without a result. */
export const throughputErrorText = (role: Role, e: NetworkTestError) => {
  switch (e) {
    case 'peer_unreachable':
      return `${peerName(role)}に届きませんでした。つながりの確認を試してください。`;
    case 'peer_busy':
      return `${peerName(role)}が別の測定をしています。少し待ってからもう一度測ってください。`;
    case 'failed':
      return '測れませんでした。';
  }
};
/** The same, short, for the list of results. */
export const throughputErrorShort = (role: Role, e: NetworkTestError) =>
  ({ peer_unreachable: `${peerName(role)}に届きませんでした`, peer_busy: `${peerName(role)}が別の測定をしていました`, failed: '測れませんでした' })[e];

/** Why the start button is not usable now (the link is not stopped). */
export const throughputBlocked = (role: Role, s: LinkState) =>
  s === 'live'
    ? '伝送中は測れません。測ると映像が止まるため、伝送を止めてから測ってください。'
    : `${stateLabel(role, s)}は測れません。伝送を止めてから測ってください。`;

/** The device refused to start a test (409 busy, 409 unsupported_role, anything else). */
export const testStartProblem = (kind: 'reachability' | 'throughput', code: string | undefined, live: boolean) => {
  if (code === 'unsupported_role') return '回線の速さは送信機の画面で測ります。';
  if (code === 'busy')
    return kind === 'reachability'
      ? 'いま別の確認をしています。少し待ってからもう一度お試しください。'
      : live
        ? '伝送中は測れません。測ると映像が止まるため、伝送を止めてから測ってください。'
        : 'ほかの測定が終わるまでお待ちください。';
  return kind === 'reachability' ? '確かめられませんでした。もう一度お試しください。' : '測れませんでした。もう一度お試しください。';
};

/**
 * What the measured rate (this device → the peer, upper limit) means for the video. The video needs
 * about its bitrate × 1.25 on the line (resends and the device's headroom); `range` is
 * VideoOptions.bitrate_range_mbps, the automatic steps are 0.5 Mbps apart.
 */
export const VIDEO_LINE_FACTOR = 1.25;
export const videoVerdict = (mbps: number, range: [number, number]): { level: 'ok' | 'lower' | 'short'; text: string } => {
  const [lo, hi] = range;
  if (mbps >= hi * VIDEO_LINE_FACTOR) return { level: 'ok', text: `おまかせの最高画質（${n1(hi)} Mbps）で送れる速さです。` };
  if (mbps >= lo * VIDEO_LINE_FACTOR) {
    const x = Math.max(lo, Math.min(hi, Math.floor((mbps / VIDEO_LINE_FACTOR) * 2 + 1e-9) / 2));
    return { level: 'lower', text: `画質を下げれば送れます（目安 ${n1(x)} Mbps まで）。` };
  }
  return { level: 'short', text: '映像を送るには足りません。' };
};

/** Why 伝送を開始 / 受信を開始 was refused. */
export const startProblem = (role: Role, code: string | undefined) =>
  code === 'busy'
    ? `回線の速さを測っているため、${role === 'tx' ? '伝送' : '受信'}を開始できません。測り終わってから開始してください。`
    : `${role === 'tx' ? '伝送' : '受信'}を開始できませんでした。もう一度お試しください。`;
