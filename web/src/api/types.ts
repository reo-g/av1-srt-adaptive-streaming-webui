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
 * low       about 1.5 s; picture quality fixed (encode.manual, whatever encode.mode says); a bad line
 *           makes the picture stop, or the receiver's delay grow, more easily. Chosen on the
 *           transmitter; the receiver follows the transmitter it is connected to.
 */
export type DelayProfile = 'standard' | 'low';

/** Fields that do not apply to the role, or cannot be measured now, are null. */
export interface Stats {
  at: string;
  delay_profile: DelayProfile; // profile in effect (rx: the transmitter's of the current or last connection)
  /**
   * Target delay of that profile (capture to display), e.g. 9500. rx: plus what the receiver has
   * added on its own since the transmitter connected (it waits longer when frames come late), to 100 ms.
   */
  delay_target_ms: number;
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
  /** Below this margin delay_margin_low is raised (about 3 s in the standard profile). null on rx and in the low profile (no margin, no warning). */
  delay_margin_warn_s: number | null;
  // rx only (null on tx)
  recv_mbps: number | null;
  stalls_5m: number | null; // times the picture stopped, last 5 min
  stall_s_5m: number | null; // total seconds the picture stood still, last 5 min
  video_latency_ms: number | null; // measured capture-to-display delay; null until it is really measured
  unrecovered_5m: number | null; // packets that could not be recovered, last 5 min
  /**
   * 受信機のモニター as the receiver reports it (OutputSettings.display): on (full screen or a window),
   * off, or failed (it could not show the picture there, e.g. nobody is logged in on the receiver's
   * screen; it tries again every 30 s while the setting is on). null on tx, while the receiver is
   * stopped and until it has said.
   */
  display: 'off' | 'on' | 'failed' | null;
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
  | 'network_test_done' // params: mbps (throughput_mbps, two decimals), direction (NetworkTestDirection). Only after a throughput test that finished
  | 'send_limit_changed' // tx; params: mbps, from_mbps, source (SendLimitSource: measured or manual)
  | 'network_settings_applied' // PUT /settings/network (途中の無線機); no params
  | 'output_settings_applied'; // rx; params: display (OutputDisplay). PUT /settings/output or POST /display

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
  | { type: 'settings'; section: 'video' | 'transport' | 'network' | 'output'; version: number };

// ---------------------------------------------------------------- shared settings types

export interface FieldError {
  /** Dotted path of the input field, list items by index from 0: "encode.manual.bitrate_mbps", "hops[1].address", "display". */
  field: string;
  code: FieldErrorCode;
  params?: Params; // min, max
}

export type FieldErrorCode =
  | 'out_of_range'
  | 'required'
  | 'too_short'
  | 'too_long'
  | 'too_many' // a list is longer than params.max (hops)
  | 'invalid'
  // hops[i].address (NetworkSettingsInput)
  | 'duplicate' // the same address as an earlier item (the later item gets it)
  | 'is_self' // an address of this device
  | 'is_peer'; // the peer's address

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
  | 'saved_while_stopped'
  // transport: only send_limit changes while the link is not stopped. Stored now, used from the next
  // start (the running transmission and its retries keep their cap)
  | 'takes_effect_next_start'
  // info, params: bitrate_mbps (encode.manual.bitrate_mbps). Low delay sends the fixed bitrate whatever
  // the video mode says: transport, delay changes to low while the video mode is auto; video, the mode
  // is auto while low is applied (not an error: the mode is kept for when the delay is standard again)
  | 'low_delay_fixed'
  // info: transport, the delay changes to low (what it means for the picture on a busy line)
  | 'low_delay_effects'
  // network settings: the list of devices on the way differs from the applied one. つながりの確認 and
  // what the radio line may carry follow the new list; the transmission is not touched
  | 'hops_changed'
  // output settings (受信機のモニター), only when the value changes. display_on: to fullscreen or window
  // (the receiver shows the picture on its monitor at once, or from the next start while stopped);
  // display_off: to off; display_fullscreen_covers: the new value is fullscreen (the monitor shows only
  // the picture: this screen cannot be used there)
  | 'display_on'
  | 'display_off'
  | 'display_fullscreen_covers';

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
  // POST /network-tests/{id}/use
  | 'not_applicable' // 409: the test is not a finished throughput test towards the peer
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
  /**
   * false while the applied transport delay is low: the fixed bitrate (encode.manual) is sent and
   * auto is not in effect, whatever encode.mode says (the stored mode is kept and applies again
   * when the delay is standard).
   */
  auto_available: boolean;
  bitrate_range_mbps: [number, number]; // [1.5, 4.0]
  encoding: VideoEncoding;
  bandwidth_mbps: number | null; // estimated line capacity, null unless the line is crowded
}

// ---------------------------------------------------------------- transport settings (both roles; the screen is tx only)

export type TransportMode = 'outbound' | 'inbound' | 'mutual'; // 送信機から接続 / 接続を待つ / 互いに接続
export type Cipher = 'aes-128' | 'aes-192' | 'aes-256' | 'none';

/**
 * Where the send cap comes from.
 * default   the device's initial value (5.0 Mbps)
 * measured  a line-speed test (POST /network-tests/{id}/use)
 * manual    entered in the transport settings
 */
export type SendLimitSource = 'default' | 'measured' | 'manual';

/**
 * 送り出しの上限: the most the transmitter puts on the line. The device starts each transmission
 * with it and lowers its pace below it on its own while the line is crowded.
 */
export interface SendLimit {
  mbps: number; // 1.0–20.0, two decimals
  source: SendLimitSource;
  measured_at: string | null; // measured: when the test finished; otherwise null
  test_id: string | null; // measured: the test it came from; otherwise null
}

export interface TransportSettings {
  version: number;
  mode: TransportMode;
  peer: { host: string; port: number };
  listen_port: number;
  /**
   * follows_peer: false on tx (chosen here); true on rx, where profile is the transmitter's of the
   * current or last connection (standard before any) and a check or PUT ignores delay.
   */
  delay: { profile: DelayProfile; follows_peer: boolean };
  encryption: {
    cipher: Cipher;
    passphrase_set: boolean; // the value itself is never returned
    passphrase_rejected_at: string | null; // the peer rejected the current value
  };
  send_limit: SendLimit;
}

export interface TransportSettingsInput {
  version: number;
  mode: TransportMode;
  peer: { host: string; port: number };
  listen_port: number;
  delay: { profile: DelayProfile }; // ignored on rx (it follows the transmitter)
  encryption: {
    cipher: Cipher;
    /** Write only. Omit to keep the current value. */
    passphrase?: string;
  };
  /**
   * Omit to keep the current value. A new value becomes source "manual"; the same value keeps its source.
   * Can be applied while live on its own (impact takes_effect_next_start): the next start uses it.
   */
  send_limit?: { mbps: number };
}

/** What can be chosen. A control with a single choice is shown as text. */
export interface TransportOptions {
  modes: TransportMode[]; // ["outbound"]
  ciphers: Cipher[]; // ["none"]
  /** available=false: shown but cannot be chosen (「準備中」); low can be turned off on the device. */
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

// ---------------------------------------------------------------- network settings (both roles)

/**
 * A device on the way to the peer (途中の無線機). つながりの確認 checks them after the peer, in
 * this order, and the radio line carries, besides the video, only the traffic with them.
 */
export interface Hop {
  label: string; // 1–24 characters after trimming, e.g. "無線機（送信機側）"
  address: string; // IPv4 address, dotted quad, e.g. "192.168.0.65" (no host names)
}

/**
 * GET /settings/network. Until the list is first applied the device returns its configured list;
 * once applied, the stored one (kept across restarts). Applying is allowed any time, also while
 * live: it does not touch the transmission.
 */
export interface NetworkSettings {
  version: number;
  hops: Hop[]; // 0–4
}

/**
 * POST /settings/network/check, PUT /settings/network (answers the new NetworkSettings).
 * The whole list. Field errors (i from 0): `hops` too_many (max 4); `hops[i].label` required /
 * too_long (max 24); `hops[i].address`, the first that applies: invalid (not a dotted quad, or
 * 0.0.0.0, 255.255.255.255, 127.x.x.x) → is_self → is_peer → duplicate.
 */
export interface NetworkSettingsInput {
  version: number;
  hops: Hop[];
}

// ---------------------------------------------------------------- output settings (rx)

/**
 * 受信機のモニター: the receiver shows the picture it receives directly on the monitor attached to it,
 * about 0.5 s sooner and smoother than the browser preview. off: not shown there; fullscreen: the
 * monitor shows only the picture; window: in a window on the receiver's screen.
 */
export type OutputDisplay = 'off' | 'fullscreen' | 'window';

/**
 * GET /settings/output (rx; a transmitter answers 409 unsupported_role for everything here).
 * Applied at once in every link state, also while live; kept across restarts.
 */
export interface OutputSettings {
  version: number;
  display: OutputDisplay; // off until something is applied
}

/**
 * POST /settings/output/check, PUT /settings/output (answers the new OutputSettings). Field error:
 * `display` invalid (not one of the modes). PUT: 409 version_conflict first.
 */
export interface OutputSettingsInput {
  version: number;
  display: OutputDisplay;
}

/**
 * POST /display (rx): the operation screen's 低遅延表示 button and the buttons that go with it. An
 * operation, not a draft: no version, no check. Sets OutputSettings.display (its version goes up, the
 * same event as a PUT) and answers the new OutputSettings. 400 invalid (`mode`) for any other value.
 */
export interface DisplayRequest {
  mode: OutputDisplay;
}

// ---------------------------------------------------------------- network test

/**
 * reachability  つながりの確認: whether the peer and the devices on the way (NetworkSettings.hops)
 *               answer, and their round-trip time. Both roles, any time (also while live).
 * throughput    回線の速さ: how fast data gets through to or from the peer. tx only, only while the
 *               link is stopped (it would disturb the video), one at a time.
 */
export type NetworkTestKind = 'reachability' | 'throughput';

/** to_peer: this device → the peer (on tx the video's direction). from_peer: the peer → this device. */
export type NetworkTestDirection = 'to_peer' | 'from_peer';

/**
 * POST /network-tests. Fields other than kind apply to throughput only; omitted ones take the defaults.
 * The throughput test sends more than the line can carry (NetworkTestOptions.saturate_mbps) and
 * measures what arrives.
 */
export interface NetworkTestRequest {
  kind: NetworkTestKind;
  direction?: NetworkTestDirection; // default to_peer
  duration_s?: number; // integer, NetworkTestOptions.duration_range_s (5–30), default 10
}

/** The request as the device runs it: defaults filled in, null where a field does not apply. */
export interface NetworkTestParams {
  kind: NetworkTestKind;
  direction: NetworkTestDirection | null; // null for reachability
  duration_s: number | null; // null for reachability
}

/** GET /network-tests/options: limits of the throughput test. */
export interface NetworkTestOptions {
  duration_range_s: [number, number]; // [5, 30]
  /** What the throughput test sends, Mbps, e.g. 6 (set on the device, not from the UI). */
  saturate_mbps: number;
}

export interface ReachabilityTarget {
  /** peer: the other device (the UI names it 受信機 / 送信機). hop: a device on the way, named by `label`. */
  kind: 'peer' | 'hop';
  label: string | null; // hop: Hop.label, e.g. "無線機（送信機側）"; peer: null
  address: string;
  sent: number; // e.g. 10
  received: number;
  /** Round-trip times; null when nothing came back. */
  rtt_min_ms: number | null;
  rtt_avg_ms: number | null;
  rtt_max_ms: number | null;
}

/**
 * Peer first (left out when its address is not known: rx before any transmitter has connected and
 * without a configured address), then the devices on the way in the order of NetworkSettings.hops
 * (the list applied when the test started). Can be empty.
 */
export interface ReachabilityResult {
  targets: ReachabilityTarget[];
}

/**
 * The test sends at about NetworkTestOptions.saturate_mbps, more than the line can carry: what
 * arrives is the line's speed. Losses are expected and not a fault.
 */
export interface ThroughputResult {
  /** 届いた速さ: what arrived at the receiving side, Mbps. */
  throughput_mbps: number;
  sent_mbps: number; // what was sent, Mbps
  loss_pct: number; // share of what was sent that did not arrive
  jitter_ms: number; // variation of the delay
  /**
   * true (loss_pct ≥ 1): the line was full and throughput_mbps is its speed. false: the line is
   * faster than the test; the UI says "at least saturate_mbps".
   */
  saturated: boolean;
  per_second_mbps: number[]; // what arrived in each second, oldest first
}

/**
 * peer_unreachable  the peer could not be reached (no route, no answer, refused)
 * peer_busy         the peer is answering another test
 * failed            anything else
 */
export type NetworkTestError = 'peer_unreachable' | 'peer_busy' | 'failed';

interface NetworkTestBase {
  id: string; // opaque
  state: 'running' | 'done' | 'failed';
  started_at: string;
  finished_at: string | null; // null while running
  progress: number; // 0–1
  error: NetworkTestError | null; // set when failed
}

/**
 * One test (POST /network-tests, GET /network-tests[/{id}]).
 * result: null while running (follow `progress`) and when failed.
 */
export type NetworkTest =
  | (NetworkTestBase & { kind: 'reachability'; request: NetworkTestParams; result: ReachabilityResult | null })
  | (NetworkTestBase & { kind: 'throughput'; request: NetworkTestParams; result: ThroughputResult | null });

/** GET /network-tests: the last 20, newest first (kept in memory only). */
export interface NetworkTestList {
  items: NetworkTest[];
}

/*
 * POST /network-tests/{id}/use (tx): makes a finished throughput test towards the peer the send cap
 * (TransportSettings.send_limit = its throughput_mbps, or saturate_mbps when not saturated, with
 * source "measured"), bumps the transport settings version and answers the new TransportSettings.
 * The next start uses it (also when used while live). Errors: 409 unsupported_role (rx),
 * 404 not_found, 409 not_applicable (not done, not throughput, or not to_peer).
 */
