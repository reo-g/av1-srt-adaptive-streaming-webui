/**
 * API contract shared by the web UI, the mock server and (by hand) the device daemon.
 * The authoritative description is api/openapi.yaml; keep this file in sync with it.
 *
 * Naming rule: every name here is product vocabulary from docs/terms.md.
 * Never expose implementation terms (protocol names, library names, pipeline
 * element names, socket option names, library error codes) in this contract.
 */

export type Role = 'tx' | 'rx';
export type Level = 'info' | 'warn' | 'error';
export type Params = Record<string, number | string>;

// ---------------------------------------------------------------- device

export interface Device {
  id: string; // "TX-01"
  role: Role;
  address: string; // "192.168.10.21"
  firmware: string; // "1.0.0"
  serial: string;
  /** IANA zone of the device clock. The header shows the time with "JST". */
  timezone: string;
}

// ---------------------------------------------------------------- status

/**
 * live         伝送中 (tx) / 受信中 (rx)
 * connecting   接続中
 * reconnecting 再接続中 (tx keeps retrying)
 * listening    待ち受け中 (rx waits for the transmitter)
 * stopped      停止中 (stopped by an operator; no retry)
 */
export type LinkState = 'live' | 'connecting' | 'reconnecting' | 'listening' | 'stopped';

export interface Peer {
  id: string | null; // "RX-02"
  address: string;
  port: number;
}

export type AlertCode =
  | 'loss_high' // params: loss_pct, threshold_pct
  | 'frames_dropped' // rx; params: frames, window_s
  | 'peer_rejected_passphrase'
  | 'peer_unreachable'
  | 'input_lost';

export interface Alert {
  level: 'warn' | 'error';
  code: AlertCode;
  since: string; // ISO 8601
  params?: Params;
}

/** What the preview shows: the input on tx, the output on rx. */
export interface Media {
  kind: 'camera' | 'file' | 'output';
  label: string; // "/dev/video0", "/srv/media/a.mp4", "SDI OUT 1"
  format: string | null; // "1080p59.94"; null while there is no signal
  preview_fps: number; // ≈5
  audio: 'none';
  black: boolean; // rx: the output is black
}

export interface Status {
  server_time: string; // ISO 8601 with offset, e.g. 2026-11-04T10:42:18+09:00
  state: LinkState;
  since: string;
  live_s: number | null; // seconds in "live"
  attempt: number | null; // reconnect attempt number
  next_retry_s: number | null;
  last_live_until: string | null; // when the last connection ended
  peer: Peer;
  listen_port: number | null; // rx
  alerts: Alert[];
  media: Media;
}

// ---------------------------------------------------------------- stats

export interface Stats {
  at: string;
  loss_pct: number | null;
  loss_threshold_pct: number; // 1.0
  rtt_ms: number | null;
  last_rtt_ms: number | null; // shown as "前回 38 ms" while rtt is unavailable
  retrans_pct: number | null; // tx 再送率 / rx 再送の割合
  buffer_ms: number; // transmission buffer in effect
  buffer_mode: 'auto' | 'manual';
  // tx only (null on rx)
  send_mbps: number | null;
  target_mbps: number | null;
  bandwidth_mbps: number | null;
  send_late_5m: number | null; // packets that could not be sent in time, last 5 min
  // rx only (null on tx)
  recv_mbps: number | null;
  frames_dropped_5m: number | null;
  video_latency_ms: number | null; // measured glass-to-output latency
  unrecovered_5m: number | null; // packets that could not be recovered, last 5 min
}

export type Metric =
  | 'send_mbps'
  | 'recv_mbps'
  | 'loss_pct'
  | 'rtt_ms'
  | 'bandwidth_mbps'
  | 'video_latency_ms'
  | 'frames_dropped';

/** Sparkline data: oldest first, `null` where nothing was measured. */
export interface StatsHistory {
  interval_s: number; // 5
  end: string;
  series: Partial<Record<Metric, (number | null)[]>>;
}

// ---------------------------------------------------------------- events

export type EventCode =
  | 'loss_high' // params: loss_pct
  | 'frames_dropped' // params: frames
  | 'peer_rejected_passphrase'
  | 'peer_disconnected'
  | 'peer_connected'
  | 'transport_started' // rx params: format
  | 'transport_stopped'
  | 'listening_started' // params: port
  | 'input_detected' // params: format
  | 'device_started'
  | 'bitrate_changed' // params: mbps
  | 'video_settings_applied'
  | 'transport_settings_applied'
  | 'passphrase_changed'
  | 'network_test_done'; // params: mbps

export interface DeviceEvent {
  id: string;
  at: string;
  level: Level;
  code: EventCode;
  params?: Params;
}

// ---------------------------------------------------------------- live channel

/** Messages pushed on the WebSocket /api/v1/live (server → browser only). */
export type LiveMessage =
  | { type: 'hello'; device: Device; status: Status; stats: Stats }
  | { type: 'status'; status: Status }
  | { type: 'stats'; stats: Stats }
  | { type: 'event'; event: DeviceEvent }
  | { type: 'settings'; section: 'video' | 'transport'; version: number };

// ---------------------------------------------------------------- shared settings types

export interface FieldError {
  /** Dotted path of the input field, e.g. "encode.manual.bitrate_mbps". */
  field: string;
  code: 'out_of_range' | 'required' | 'too_short' | 'too_long' | 'invalid';
  params?: Params; // min, max
}

export type ImpactCode =
  // video
  | 'input_switch_gap'
  | 'encoder_restart_gap'
  | 'connection_kept'
  | 'manual_fixed'
  | 'auto_decides'
  | 'bitrate_lower_quality'
  | 'bitrate_near_bandwidth' // params: bandwidth_mbps
  // transport
  | 'reconnect'
  | 'reconnect_gap'
  | 'no_peer_effect'
  | 'saved_while_stopped'
  | 'buffer_below_guide'; // params: guide_ms

export interface Impact {
  level: 'warn' | 'ok' | 'info';
  code: ImpactCode;
  params?: Params;
}

/** Result of POST …/check: validation plus what applying would do. Nothing is changed. */
export interface CheckResult {
  ok: boolean;
  errors: FieldError[];
  impacts: Impact[];
}

export interface ApplyResult<T> {
  settings: T;
  applied_at: string;
  impacts: Impact[];
}

export interface ApiError {
  error: { code: 'version_conflict' | 'invalid' | 'busy' | 'not_found' | 'unsupported_role'; errors?: FieldError[] };
}

// ---------------------------------------------------------------- video settings (tx)

export interface EncodeParams {
  resolution: 'source' | '1280x720';
  framerate: 'source' | '29.97';
  bitrate_mbps: number;
  rate_control: 'cbr' | 'vbr';
  codec: 'h264' | 'h265';
  profile: 'high' | 'main' | 'baseline';
  keyframe_s: number;
  bframes: 0 | 1 | 2;
  encoder: 'hardware' | 'software';
}

export interface VideoSettings {
  version: number;
  input: {
    kind: 'camera' | 'file';
    camera: string; // "/dev/video0"
    file: string; // "/srv/media/colorbars_1080p59.mp4"
    loop: boolean;
  };
  encode: {
    mode: 'auto' | 'manual';
    /** Values used in manual mode. Kept while in auto so switching back restores them. */
    manual: EncodeParams;
    /** Read only: what the device chose in auto mode right now. */
    auto: EncodeParams;
  };
  audio: { enabled: false };
}

export interface VideoSettingsInput {
  version: number;
  input: VideoSettings['input'];
  encode: { mode: 'auto' | 'manual'; manual: EncodeParams };
}

export interface VideoOptions {
  cameras: { device: string; label: string; signal: string | null }[]; // signal "1920×1080　59.94p"
  files: { path: string; format: string; duration_s: number }[];
  bitrate_range_mbps: [number, number]; // [1, 40]
  keyframe_range_s: [number, number]; // [0.5, 10]
  bandwidth_mbps: number | null; // for the hint "回線の推定帯域は 52 Mbps"
}

// ---------------------------------------------------------------- transport settings (tx)

export type TransportMode = 'outbound' | 'inbound' | 'mutual'; // 発信 / 待ち受け / 相互接続
export type Cipher = 'aes-128' | 'aes-192' | 'aes-256' | 'none';

export interface TransportSettings {
  version: number;
  mode: TransportMode;
  peer: { host: string; port: number };
  listen_port: number;
  buffer: {
    mode: 'auto' | 'manual';
    manual_ms: number;
    auto_ms: number; // what auto mode uses now
    last_rtt_ms: number | null; // RTT the auto value was based on
    guide_min_ms: number | null; // 4 × RTT
  };
  encryption: {
    cipher: Cipher;
    passphrase_set: boolean; // the value itself is never returned
    passphrase_rejected_at: string | null; // the peer rejected the current value
  };
}

export interface TransportSettingsInput {
  version: number;
  mode: TransportMode;
  peer: { host: string; port: number };
  listen_port: number;
  buffer: { mode: 'auto' | 'manual'; manual_ms: number };
  encryption: {
    cipher: Cipher;
    /** Write only. Omit to keep the current value. */
    passphrase?: string;
  };
}

export interface TransportOptions {
  buffer_presets_ms: number[]; // [80, 120, 160, 200, 300, 500, 1000]
}

export type CompareRow =
  | { key: 'mode'; local: TransportMode; peer: TransportMode | null; verdict: 'compatible' | 'incompatible' | 'unknown' }
  | {
      key: 'endpoint';
      local: { host: string; port: number };
      peer: { listen_port: number } | null;
      verdict: 'match' | 'mismatch' | 'unknown';
    }
  | {
      key: 'buffer';
      local_ms: number;
      local_mode: 'auto' | 'manual';
      peer_ms: number | null;
      effective_ms: number | null; // the larger of the two is used
      verdict: 'effective' | 'unknown';
    }
  | { key: 'cipher'; local: Cipher; peer: Cipher | null; verdict: 'match' | 'mismatch' | 'unknown' }
  | { key: 'passphrase'; local_set: boolean; verdict: 'match' | 'mismatch' | 'unknown'; reason: 'peer_rejected' | null };

/** Settings of the peer as learned when the link was last negotiated. */
export interface CompareResult {
  received_at: string | null;
  peer_id: string | null;
  rows: CompareRow[];
}

// ---------------------------------------------------------------- network test (tx starts it)

export interface NetworkTestRequest {
  direction: 'to_peer' | 'from_peer';
  duration_s: number; // 5–30
}

export interface NetworkTest {
  id: string;
  state: 'running' | 'done' | 'failed';
  direction: 'to_peer' | 'from_peer';
  started_at: string;
  progress: number; // 0–1
  result: { throughput_mbps: number; loss_pct: number; jitter_ms: number } | null;
  error: 'peer_unreachable' | 'busy' | null;
}
