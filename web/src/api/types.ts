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
  | 'delay_margin_low' // tx; params: margin_s. The margin before the receiver's picture would stop fell below about 3 s
  | 'video_stalled' // rx; params: count, seconds, window_s. The picture stopped in the last window_s seconds
  | 'loss_high' // params: loss_pct, threshold_pct. Not raised in the standard delay profile (losses are recovered by resending)
  | 'peer_rejected_passphrase' // kept for when encryption is available
  | 'peer_unreachable'
  | 'input_lost';

export interface Alert {
  level: 'warn' | 'error';
  code: AlertCode;
  since: string; // ISO 8601
  params?: Params;
}

/** The picture of the operation screen: the input on tx (the preview shows it as sent), the output on rx. */
export interface Media {
  kind: 'camera' | 'file' | 'output';
  label: string; // what the operator recognises: camera name "Facecam 4K", file name "sample_2160p30.y4m", "OUT 1"
  format: string | null; // "3840×2160　30p" (U+3000 between); null while there is no signal
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

/**
 * standard  about 10 s from capture to display; tolerant of a bad line; automatic quality works
 * low       about 1.5 s; picture quality fixed; a bad line makes the picture stop more easily
 */
export type DelayProfile = 'standard' | 'low';

/** Fields that do not apply to the role, or cannot be measured now, are null. */
export interface Stats {
  at: string;
  delay_profile: DelayProfile; // profile in effect
  delay_target_ms: number; // target delay of that profile (capture to display), e.g. 9500
  loss_pct: number | null; // line loss; tx: resent ÷ sent packets, rx: lost ÷ expected packets
  /** Above this loss_high is raised. null while losses are recovered by resending (standard profile). */
  loss_threshold_pct: number | null;
  rtt_ms: number | null;
  last_rtt_ms: number | null; // shown as "前回 38 ms" while rtt is unavailable
  retrans_pct: number | null; // rx only: share of packets that arrived by resending (null on tx)
  // tx only (null on rx)
  send_mbps: number | null; // what goes out on the line, resends included
  target_mbps: number | null; // video bitrate the device aims at now
  actual_mbps: number | null; // video bitrate actually produced (5 s mean)
  bandwidth_mbps: number | null; // estimated line capacity; only measurable while the line is crowded
  send_late_5m: number | null; // packets that could not be sent in time, last 5 min
  delay_margin_s: number | null; // seconds left before the receiver's picture would stop (≈6 s normally); 1 decimal rounded down, same as delay_margin_low.margin_s
  /** Below this margin delay_margin_low is raised (about 3 s in the standard profile). */
  delay_margin_warn_s: number | null;
  // rx only (null on tx)
  recv_mbps: number | null;
  stalls_5m: number | null; // times the picture stopped, last 5 min
  stall_s_5m: number | null; // total seconds the picture stood still, last 5 min
  video_latency_ms: number | null; // measured capture-to-display delay; null until it is really measured
  unrecovered_5m: number | null; // packets that could not be recovered, last 5 min
}

export type Metric =
  | 'send_mbps'
  | 'recv_mbps'
  | 'target_mbps'
  | 'actual_mbps'
  | 'loss_pct'
  | 'rtt_ms'
  | 'bandwidth_mbps'
  | 'delay_margin_s' // tx
  | 'stall_s'; // rx: seconds the picture stood still in each interval

/** Sparkline data: oldest first, `null` where nothing was measured. */
export interface StatsHistory {
  interval_s: number; // 5
  end: string;
  series: Partial<Record<Metric, (number | null)[]>>;
}

// ---------------------------------------------------------------- events

export type EventCode =
  | 'delay_margin_low' // tx; params: margin_s
  | 'video_stalled' // rx; params: seconds
  | 'loss_high' // params: loss_pct
  | 'peer_rejected_passphrase'
  | 'peer_disconnected'
  | 'peer_connected'
  | 'transport_started' // rx params: format
  | 'transport_stopped'
  | 'listening_started' // params: port
  | 'input_detected' // params: format
  | 'device_started'
  | 'bitrate_changed' // params: mbps, from_mbps, reason (BitrateReason)
  | 'video_settings_applied'
  | 'transport_settings_applied'
  | 'passphrase_changed'
  | 'network_test_done'; // params: mbps

/**
 * Why the video bitrate changed (bitrate_changed.reason).
 * loss       the line got crowded and losses grew (automatic, down)
 * delay      sending fell behind (automatic, down)
 * recovered  the line has room again (automatic, up)
 * manual     an operator changed the bitrate or the mode
 */
export type BitrateReason = 'loss' | 'delay' | 'recovered' | 'manual';

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
  /** Dotted path of the input field, e.g. "encode.manual.bitrate_mbps", "delay.profile". */
  field: string;
  code: 'out_of_range' | 'required' | 'too_short' | 'too_long' | 'invalid';
  params?: Params; // min, max
}

export type ImpactCode =
  // video
  | 'input_switch_gap' // params: gap_s; reconnect_gap comes with it (the UI shows the pair as one line)
  | 'switch_no_gap'
  | 'manual_fixed'
  | 'auto_decides'
  | 'bitrate_lower_quality'
  | 'bitrate_near_bandwidth' // params: bandwidth_mbps
  // transport (reconnect_gap also when the input changes while live)
  | 'reconnect'
  | 'reconnect_gap' // params: gap_s
  | 'no_peer_effect'
  | 'saved_while_stopped';

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

export type ApiErrorCode =
  | 'version_conflict'
  | 'invalid'
  | 'busy'
  | 'not_found'
  | 'unsupported_role'
  // POST /preview/sessions
  | 'not_live' // 409: the preview exists only while the link is live
  | 'too_many_viewers' // 429: other screens are already watching (the device limits the number)
  | 'bad_offer' // 400: the offer is missing, empty or not a session description
  | 'no_common_codec' // 400: the browser cannot play any of the preview's video formats
  | 'preview_unavailable'; // 503: the device could not prepare the preview now

export interface ApiError {
  error: { code: ApiErrorCode; errors?: FieldError[] };
}

// ---------------------------------------------------------------- preview

/**
 * Live preview in the browser (POST /preview/sessions). tx shows the picture as it is sent
 * (after compression), rx the picture it is showing. Only while the link is live.
 * The browser makes a receive-only offer with its candidates already gathered; the
 * device answers in one step (no trickle). Close it with DELETE /preview/sessions/{id}.
 */
export interface PreviewOffer {
  offer: string; // session description text, starts with "v=0"; at most 64 KB
}

export interface PreviewSession {
  id: string; // opaque
  answer: string; // session description text
}

// ---------------------------------------------------------------- video settings (tx)

/** The only encoding parameter that can be chosen. Everything else is fixed (VideoOptions.encoding). */
export interface EncodeParams {
  bitrate_mbps: number;
}

export interface VideoSettings {
  version: number;
  input: {
    kind: 'camera' | 'file';
    camera: string; // "/dev/video0"
    file: string; // "/srv/media/sample_2160p30.y4m"
    loop: boolean; // only when VideoOptions.loop_available
  };
  encode: {
    /** auto: the device lowers the bitrate when the line gets crowded and raises it when there is room. manual: fixed. */
    mode: 'auto' | 'manual';
    /** The fixed bitrate. Kept while in auto so switching back restores it. */
    manual: EncodeParams;
    /** Read only: the bitrate auto mode aims at right now (the live value is Stats.target_mbps). */
    auto: EncodeParams;
  };
  audio: { enabled: false };
}

export interface VideoSettingsInput {
  version: number;
  input: VideoSettings['input'];
  encode: { mode: 'auto' | 'manual'; manual: EncodeParams };
}

/** Fixed encoding: shown read-only. "source" = the same as the input. */
export interface VideoEncoding {
  codec: 'av1';
  resolution: 'source';
  framerate: 'source';
  keyframe_s: number; // 2
}

export interface VideoOptions {
  cameras: { device: string; label: string; signal: string | null }[]; // signal "3840×2160　30p" (U+3000 between)
  files: { path: string; format: string; duration_s: number }[]; // format "3840×2160　30p"
  loop_available: boolean; // false: files play once
  bitrate_range_mbps: [number, number]; // [1.5, 4.0]
  encoding: VideoEncoding;
  bandwidth_mbps: number | null; // estimated line capacity, null unless the line is crowded
}

// ---------------------------------------------------------------- transport settings (tx)

export type TransportMode = 'outbound' | 'inbound' | 'mutual'; // 送信機から接続 / 接続を待つ / 互いに接続
export type Cipher = 'aes-128' | 'aes-192' | 'aes-256' | 'none';

export interface TransportSettings {
  version: number;
  mode: TransportMode;
  peer: { host: string; port: number };
  listen_port: number;
  delay: { profile: DelayProfile };
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
  delay: { profile: DelayProfile };
  encryption: {
    cipher: Cipher;
    /** Write only. Omit to keep the current value. */
    passphrase?: string;
  };
}

/** What can be chosen. A control with a single choice is shown as text. */
export interface TransportOptions {
  modes: TransportMode[]; // ["outbound"]
  ciphers: Cipher[]; // ["none"]
  /** available=false: shown but cannot be chosen yet (「準備中」). */
  delay_profiles: { profile: DelayProfile; target_ms: number; available: boolean }[];
}

export type CompareRow =
  | { key: 'mode'; local: TransportMode; peer: TransportMode | null; verdict: 'compatible' | 'incompatible' | 'unknown' }
  | {
      key: 'endpoint';
      local: { host: string; port: number };
      peer: { listen_port: number } | null;
      verdict: 'match' | 'mismatch' | 'unknown';
    }
  | { key: 'delay'; local: DelayProfile; peer: DelayProfile | null; verdict: 'match' | 'mismatch' | 'unknown' }
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
