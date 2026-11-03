/**
 * Every user-facing sentence that depends on API codes lives here.
 * Wording follows docs/terms.md (用語の言い換え対応表): do not introduce
 * implementation terms. The API sends codes and numbers; the UI owns the words.
 */
import type {
  Alert,
  Cipher,
  DeviceEvent,
  FieldError,
  Impact,
  LinkState,
  Role,
  TransportMode,
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

export const alertText = (role: Role, a: Alert, peerId: string | null): string => {
  const p = a.params ?? {};
  switch (a.code) {
    case 'loss_high':
      return `回線のパケット損失が基準を超えています（${n1(p.loss_pct)}%）。`;
    case 'frames_dropped':
      return `直近 5 分で映像が ${p.frames} フレーム欠落しました。`;
    case 'peer_rejected_passphrase':
      return role === 'tx'
        ? `${peerOf(role, peerId)} が接続を拒否しています。パスフレーズが一致していません。`
        : `${peerOf(role, peerId)} からの接続を拒否しました。パスフレーズが一致していません。`;
    case 'peer_unreachable':
      return `${peerOf(role, peerId)} に接続できません。ケーブルと受信機の電源を確認してください。`;
    case 'input_lost':
      return '入力信号を検出できません。カメラの接続を確認してください。';
  }
};

export const eventText = (role: Role, e: DeviceEvent, peerId: string | null): string => {
  const p = e.params ?? {};
  switch (e.code) {
    case 'loss_high':
      return `パケット損失率が基準を超えました（${n1(p.loss_pct)}%）`;
    case 'frames_dropped':
      return `映像の欠落を検出しました（${p.frames} フレーム）`;
    case 'peer_rejected_passphrase':
      return role === 'tx'
        ? `${peerOf(role, peerId)} が接続を拒否しました（パスフレーズの不一致）`
        : `${peerOf(role, peerId)} からの接続を拒否しました（パスフレーズの不一致）`;
    case 'peer_disconnected':
      return `${peerOf(role, peerId)} との接続が切れました`;
    case 'peer_connected':
      return role === 'tx' ? `${peerOf(role, peerId)} に接続しました` : `${peerOf(role, peerId)} から接続されました`;
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
      return `映像ビットレートを ${n1(p.mbps)} Mbps に変更しました`;
    case 'video_settings_applied':
      return '映像設定を適用しました';
    case 'transport_settings_applied':
      return '伝送設定を適用しました';
    case 'passphrase_changed':
      return 'パスフレーズを変更しました';
    case 'network_test_done':
      return `回線テストが終わりました（${n1(p.mbps)} Mbps）`;
  }
};

export const impactText = (i: Impact): string => {
  const p = i.params ?? {};
  switch (i.code) {
    case 'input_switch_gap':
      return '入力を切り替える間、受信側の映像が約 1 秒途切れます';
    case 'encoder_restart_gap':
      return '伝送中のため、受信側の映像が約 2 秒途切れます（エンコーダの再初期化）';
    case 'connection_kept':
      return '接続は維持されます。再接続は起きません';
    case 'manual_fixed':
      return '手動の値は、入力や回線が変わっても自動では変わりません';
    case 'auto_decides':
      return '解像度・フレームレート・ビットレートは機器が決めます';
    case 'bitrate_lower_quality':
      return 'ビットレートを下げると、動きの多い場面で画質が下がることがあります';
    case 'bitrate_near_bandwidth':
      return `推定帯域 ${p.bandwidth_mbps} Mbps に対して余裕が少なくなり、損失が増えるおそれがあります`;
    case 'reconnect':
      return '接続をやり直します';
    case 'reconnect_gap':
      return '接続をやり直すため、受信側の映像が数秒途切れます';
    case 'no_peer_effect':
      return 'いまは未接続のため、受信側への追加の影響はありません';
    case 'saved_while_stopped':
      return '設定を保存します。伝送は停止したままです';
    case 'buffer_below_guide':
      return `伝送バッファが目安の ${p.guide_ms} ms より短く、損失が増えると映像が乱れやすくなります`;
  }
};

export const fieldErrorText = (e: FieldError): string => {
  const p = e.params ?? {};
  switch (e.field) {
    case 'encode.manual.bitrate_mbps':
      return `${p.min}〜${p.max} の数値を入力してください`;
    case 'encode.manual.keyframe_s':
      return `${p.min}〜${p.max} の数値を入力してください`;
    case 'encryption.passphrase':
      // The allowed length is shown only when the value is out of range (docs/terms.md).
      return `${p.min}〜${p.max} 文字で入力してください`;
    case 'peer.host':
      return 'IP アドレスの形式で入力してください';
    case 'peer.port':
    case 'listen_port':
      return '1〜65535 の数値を入力してください';
    default:
      return '入力内容を確認してください';
  }
};

export const modeName = (m: TransportMode) => ({ outbound: '発信', inbound: '待ち受け', mutual: '相互接続' })[m];
export const cipherName = (c: Cipher) =>
  ({ 'aes-128': 'AES-128', 'aes-192': 'AES-192', 'aes-256': 'AES-256', none: 'なし' })[c];
