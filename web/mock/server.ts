/**
 * Mock device API for UI development (npm run mock).
 *
 * Implements api/openapi.yaml with simulated data so every screen can be
 * exercised without a camera, encoder or a second device. The real daemon
 * replaces this file; nothing here ships on the device.
 *
 *   MOCK_ROLE=tx|rx            which device to imitate (default tx)
 *   MOCK_SCENARIO=normal|warn|error   (default warn)
 *   MOCK_PORT=8787
 *   MOCK_LINE_FAST=1           the line is faster than the line-speed test sends (result 「6 Mbps 以上」)
 *   MOCK_PEER_DELAY=low|standard  rx: the delay of the transmitter it follows (default low)
 *   MOCK_DISPLAY=ok|failed     rx: whether the receiver can show the picture on its monitor (default ok)
 *
 * Scenarios (they imitate the real system: automatic bitrate between 1.5 and 4.0 Mbps,
 * standard delay profile of about 10 s, losses recovered by resending):
 *   normal  the line is mostly fine; now and then a short crowded spell makes automatic mode
 *           step 4.0 → 3.5 Mbps, and it steps back up later
 *   warn    the line gets crowded: the margin before the picture would stop falls below 3 s,
 *           automatic mode steps down 3.5 → 2.5 Mbps, then back up; rx shows picture stops
 * While the line is crowded the transmitter sends at about the line's capacity (the backlog is
 * what shrinks the margin) and the capacity can be estimated; otherwise it is unknown (null).
 * The produced video rate follows a new target over a few seconds.
 *   error   the receiver cannot be reached: tx keeps reconnecting, rx waits for a caller
 *
 * 遅延 (delay): the transmitter offers 標準 and 低遅延. While low is applied it sends the fixed
 * bitrate (encode.manual) whatever the video mode says, without automatic steps, margin or margin
 * warning (VideoOptions.auto_available false); the stored video mode is kept. The receiver follows
 * the transmitter: here a fixed value (MOCK_PEER_DELAY), shown in its stats and its transport
 * settings (delay.follows_peer), and its check and PUT ignore delay.
 *
 * 途中の無線機 (GET/PUT /settings/network, both roles): starts with the two radios of the real
 * installation (the one next to this device first) at version 1, checks drafts with the device's
 * rules and stores them like the device (also while live; nothing else changes). つながりの確認 checks
 * the applied list.
 *
 * 受信機のモニター (rx: GET/PUT /settings/output, the check, POST /display): off at version 1, the
 * device's rules; applied at once in every state. The receiver reports it in Stats.display: off,
 * on, or failed with MOCK_DISPLAY=failed (as without a desktop session), null while stopped.
 *
 * The preview cannot be imitated: POST /preview/sessions answers 503 preview_unavailable while
 * live, so the screens show their "cannot show the preview" overlay.
 *
 * Development-only endpoints: POST /mock/role {role}, POST /mock/scenario {scenario}
 */
import http, { type IncomingMessage, type ServerResponse } from 'node:http';
import { WebSocket, WebSocketServer } from 'ws';
import type {
  Alert,
  ApiError,
  ApplyResult,
  BitrateReason,
  CheckResult,
  CompareResult,
  DelayProfile,
  Device,
  DeviceEvent,
  DisplayRequest,
  EventCode,
  FieldError,
  Hop,
  Impact,
  Level,
  LiveMessage,
  Media,
  Metric,
  NetworkSettings,
  NetworkSettingsInput,
  NetworkTest,
  NetworkTestDirection,
  NetworkTestList,
  NetworkTestOptions,
  NetworkTestParams,
  NetworkTestRequest,
  OutputDisplay,
  OutputSettings,
  OutputSettingsInput,
  Params,
  ReachabilityTarget,
  PreviewOffer,
  Role,
  Stats,
  StatsHistory,
  Status,
  TransportOptions,
  TransportSettings,
  TransportSettingsInput,
  VideoOptions,
  VideoSettings,
  VideoSettingsInput,
} from '../src/api/types.ts';

type Scenario = 'normal' | 'warn' | 'error';

const PORT = Number(process.env.MOCK_PORT ?? 8787);
let role: Role = process.env.MOCK_ROLE === 'rx' ? 'rx' : 'tx';
let scenario: Scenario = (['normal', 'warn', 'error'] as const).includes(process.env.MOCK_SCENARIO as Scenario)
  ? (process.env.MOCK_SCENARIO as Scenario)
  : 'warn';

// ------------------------------------------------------------------ time helpers

/** ISO 8601 in JST with offset, without milliseconds. */
const iso = (ms: number) => new Date(ms + 9 * 3600e3).toISOString().replace(/\.\d{3}Z$/, '+09:00');
const now = () => Date.now();
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const mod = (a: number, n: number) => ((a % n) + n) % n;
const r1 = (v: number) => Math.round(v * 10) / 10;
const r2 = (v: number) => Math.round(v * 100) / 100;
/** One decimal, rounded down: a margin that just fell below 3 s reads 2.9, not 3.0. */
const f1 = (v: number) => Math.floor(v * 10) / 10;

// ------------------------------------------------------------------ fixed options

const LADDER = [1.5, 2.0, 2.5, 3.0, 3.5, 4.0];
const SIGNAL_4K30 = '3840×2160　30p';
const MARGIN_WARN_S = 3;
/** Seconds the produced video rate takes to reach a new target. */
const RAMP_S = 3;
const STALL_WINDOW_S = 300;

const OPTIONS: Omit<VideoOptions, 'bandwidth_mbps' | 'auto_available'> = {
  cameras: [{ device: '/dev/video0', label: 'Facecam 4K', signal: SIGNAL_4K30 }],
  files: [
    { path: '/srv/media/big_buck_bunny_2160p30.y4m', format: SIGNAL_4K30, duration_s: 634 },
    { path: '/srv/media/test_pattern_2160p30.y4m', format: SIGNAL_4K30, duration_s: 60 },
  ],
  loop_available: false,
  bitrate_range_mbps: [LADDER[0], LADDER[LADDER.length - 1]],
  encoding: { codec: 'av1', resolution: 'source', framerate: 'source', keyframe_s: 2 },
};

const TRANSPORT_OPTIONS: TransportOptions = {
  modes: ['outbound'],
  ciphers: ['none'],
  delay_profiles: [
    { profile: 'standard', target_ms: 9500, available: true },
    { profile: 'low', target_ms: 1500, available: true },
  ],
};
const targetMsOf = (p: DelayProfile) => TRANSPORT_OPTIONS.delay_profiles.find((d) => d.profile === p)?.target_ms ?? 9500;
/** Seconds the receiver's picture is gone when the link is set up again: about the delay of the profile. */
const reconnectGapS = (p: DelayProfile) => Math.round(targetMsOf(p) / 1000);
const baseName = (path: string) => path.slice(path.lastIndexOf('/') + 1);

/** rx: the delay of the transmitter it follows (the mock has no transmitter: a fixed value). */
const RX_PEER_DELAY: DelayProfile = process.env.MOCK_PEER_DELAY === 'standard' ? 'standard' : 'low';
const PEER_CIPHER = 'none';

/** 送り出しの上限: the device's initial value and the range it accepts (Mbps, two decimals). */
const SEND_LIMIT_DEFAULT = 5.0;
const SEND_LIMIT_RANGE: [number, number] = [1, 20];

// ------------------------------------------------------------------ state

let t0 = 0; // model time origin (reset)
let liveSince = 0;
let stoppedAt: number | null = null;
let attempt = 5;
let nextRetryAt = 0;
let lastLiveUntil = 0;
let negotiatedAt = 0;
let events: DeviceEvent[] = [];
let eventSeq = 0;
let history: Record<Metric, (number | null)[]>;
let tick = 0;
let lastAutoTarget: number | null = null;
let wasMarginLow = false;
let lastTickT = 0;
/** Last applied change of the fixed bitrate or the mode (model time), so the produced rate can ramp. */
let switchRamp: { at: number; from: number } | null = null;

let video: VideoSettings;
let transport: TransportSettings;
let network: NetworkSettings;
let output: OutputSettings;

const modelT = (ms = now()) => (ms - t0) / 1000;

const device = (): Device =>
  role === 'tx'
    ? { id: 'TX-01', role, address: '192.168.10.21', firmware: '1.0.0', serial: 'VT-2026-000123', timezone: 'Asia/Tokyo' }
    : { id: 'RX-02', role, address: '192.168.10.22', firmware: '1.0.0', serial: 'VR-2026-000456', timezone: 'Asia/Tokyo' };

const peer = () =>
  role === 'tx' ? { id: 'RX-02', address: transport.peer.host, port: transport.peer.port } : { id: 'TX-01', address: '10.20.1.21', port: 9000 };

// ------------------------------------------------------------------ line model

/**
 * The line and what automatic mode does on it, as a function of model time T (seconds).
 * Periodic, so the past (history, events) can be evaluated the same way as the present.
 */
interface Line {
  target: number; // automatic target, Mbps (a LADDER step)
  reason: BitrateReason; // why automatic mode is at this step
  margin: number; // seconds before the receiver's picture would stop
  crowded: boolean; // the sender is backlogged and sends at about the capacity: measurable, losses higher
  capacity: number; // line capacity while crowded (Mbps)
}

function line(T: number): Line {
  if (scenario === 'warn') {
    // 180 s cycle: congestion, step down for delay, the backlog drains, the crowding ends,
    // two steps back up, congestion again.
    const c = mod(T + 3, 180);
    if (c < 15) return { target: 3.5, reason: 'recovered', margin: 2.9 - 0.06 * c, crowded: true, capacity: 2.8 };
    if (c < 75) return { target: 2.5, reason: 'delay', margin: 2.0 + 0.025 * (c - 15), crowded: true, capacity: 2.8 };
    if (c < 105) return { target: 2.5, reason: 'delay', margin: 3.5 + 0.1 * (c - 75), crowded: true, capacity: 2.8 };
    if (c < 135) return { target: 3.0, reason: 'recovered', margin: 6.5, crowded: false, capacity: 2.8 };
    if (c < 140) return { target: 3.5, reason: 'recovered', margin: 6.5, crowded: false, capacity: 2.8 };
    return { target: 3.5, reason: 'recovered', margin: 6.5 - 0.09 * (c - 140), crowded: true, capacity: 2.8 };
  }
  // normal: 240 s cycle, a short crowded spell (capacity below what 4.0 Mbps needs) makes automatic
  // mode step 4.0 → 3.5; the backlog drains, and 75 s after the spell it steps back up.
  const c = mod(T + 100, 240);
  const crowded = c >= 140 && c < 165;
  const dip = !crowded ? 0 : c < 150 ? 0.06 * (c - 140) : Math.max(0, 0.6 - 0.04 * (c - 150));
  return c < 150
    ? { target: 4.0, reason: 'recovered', margin: 6.6 - dip, crowded, capacity: 3.8 }
    : { target: 3.5, reason: 'loss', margin: 6.6 - dip, crowded, capacity: 3.8 };
}

const jit = (T: number, a: number, p: number) => a * (0.6 * Math.sin(T * 0.9 + p) + 0.4 * Math.sin(T * 0.23 + 2 * p));

/** The delay in effect: tx the applied transport settings', rx the transmitter's it follows. */
const delayInEffect = (): DelayProfile => (role === 'tx' ? transport.delay.profile : RX_PEER_DELAY);
/** tx: low delay is applied, so the fixed bitrate is sent whatever the video mode says. */
const lowApplied = () => role === 'tx' && transport.delay.profile === 'low';
const autoInEffect = () => video.encode.mode === 'auto' && !lowApplied();
const autoTarget = (T = modelT()) => line(T).target;
const effectiveTarget = (T = modelT()) => (autoInEffect() ? autoTarget(T) : video.encode.manual.bitrate_mbps);

/** The rate the encoder aims at after a change: it reaches a new target over RAMP_S seconds. */
function rampedTarget(T: number): number {
  const target = effectiveTarget(T);
  if (switchRamp && T >= switchRamp.at && T < switchRamp.at + RAMP_S)
    return switchRamp.from + ((target - switchRamp.from) * (T - switchRamp.at)) / RAMP_S;
  if (!autoInEffect()) return target;
  // Automatic steps: the mean of the target over the last RAMP_S seconds is a ramp after each step.
  const k = 6;
  let sum = 0;
  for (let i = 0; i <= k; i++) sum += line(T - (RAMP_S * i) / k).target;
  return sum / (k + 1);
}

/** Seconds before the receiver's picture would stop, as the readout, the alert and the events show it. */
function marginAt(T: number): number {
  const m = line(T).margin;
  return f1(Math.max(0, m + (m > 5 ? jit(T, 0.08, 5) : 0)));
}

/** When the margin fell below the warning level (model time), or null when it is not low. */
function marginLowSince(T: number): number | null {
  if (role !== 'tx' || lowApplied() || line(T).margin >= MARGIN_WARN_S) return null;
  let k = 1;
  while (k < 300 && line(T - k).margin < MARGIN_WARN_S) k++;
  return T - k + 1;
}

/** rx in the warn scenario: the picture stops now and then (every 150 s, 0.5 or 0.8 s). */
const STALL_PERIOD = 150;
const STALL_PHASE = -101;
const stallDur = (k: number) => (mod(k, 2) === 0 ? 0.5 : 0.8);
function stallsIn(from: number, to: number): { at: number; seconds: number }[] {
  if (role !== 'rx' || scenario !== 'warn') return [];
  const out: { at: number; seconds: number }[] = [];
  for (let k = Math.ceil((from - STALL_PHASE) / STALL_PERIOD); STALL_PHASE + k * STALL_PERIOD <= to; k++) {
    const at = STALL_PHASE + k * STALL_PERIOD;
    if (at > from) out.push({ at, seconds: stallDur(k) });
  }
  return out;
}

type Sample = Partial<Record<Metric, number | null>>;

function sample(T: number): Sample {
  const l = line(T);
  const loss = Math.max(0, (l.crowded ? (scenario === 'warn' ? 4.2 : 2.4) : scenario === 'warn' ? 0.8 : 0.4) + jit(T, 0.25, 2));
  const target = effectiveTarget(T);
  const actual = rampedTarget(T) * 0.94 + jit(T, 0.06, 1);
  // Backlogged: the line carries about its capacity (more than produced while the backlog drains,
  // less while it grows). Otherwise: what is produced plus the resends.
  const capacity = l.crowded ? l.capacity + jit(T, 0.08, 4) : null;
  const wire = capacity !== null ? capacity * 0.96 + jit(T, 0.02, 6) : actual * (1 + loss / 100) + 0.04;
  const rtt = (l.crowded ? 24 : 18) + jit(T, 1.2, 3);
  if (role === 'tx')
    return {
      target_mbps: target,
      actual_mbps: actual,
      send_mbps: wire,
      loss_pct: loss,
      rtt_ms: rtt,
      bandwidth_mbps: capacity,
      // low delay: no margin (and no warning)
      delay_margin_s: lowApplied() ? null : marginAt(T),
    };
  return {
    recv_mbps: wire - 0.01,
    loss_pct: loss,
    rtt_ms: rtt,
    stall_s: 0,
  };
}

// ------------------------------------------------------------------ settings defaults

function reset() {
  const t = now();
  t0 = t;
  liveSince = t - (42 * 60 + 13) * 1000;
  negotiatedAt = liveSince - 1000;
  stoppedAt = null;
  attempt = 5;
  nextRetryAt = t + 4000;
  lastLiveUntil = t - 20000;
  video = {
    version: 1,
    input: { kind: 'camera', camera: '/dev/video0', file: OPTIONS.files[0].path, loop: false },
    encode: { mode: 'auto', manual: { bitrate_mbps: 3.0 }, auto: { bitrate_mbps: line(0).target } },
    audio: { enabled: false },
  };
  transport = {
    version: 1,
    mode: 'outbound',
    peer: { host: '10.20.1.15', port: 9000 },
    listen_port: 9000,
    delay: { profile: 'standard', follows_peer: false },
    encryption: { cipher: 'none', passphrase_set: false, passphrase_rejected_at: null },
    send_limit: { mbps: SEND_LIMIT_DEFAULT, source: 'default', measured_at: null, test_id: null },
  };
  // What the device's configuration file lists, until the operator applies a list.
  network = { version: 1, hops: defaultHops() };
  output = { version: 1, display: 'off' };
  lastAutoTarget = autoTarget(0);
  wasMarginLow = scenario === 'warn' && marginLowSince(0) !== null;
  lastTickT = 0;
  switchRamp = null;
  tests = [];
  seedEvents();
  seedHistory();
}

// ------------------------------------------------------------------ status

const isLive = () => stoppedAt === null && scenario !== 'error';

function alerts(): Alert[] {
  if (stoppedAt !== null) return [];
  const t = now();
  if (scenario === 'error') return role === 'tx' ? [{ level: 'error', code: 'peer_unreachable', since: iso(lastLiveUntil + 3000) }] : [];
  const T = modelT(t);
  const list: Alert[] = [];
  const low = marginLowSince(T);
  if (low !== null)
    list.push({ level: 'warn', code: 'delay_margin_low', since: iso(t0 + low * 1000), params: { margin_s: marginAt(T) } });
  const stalls = stallsIn(T - STALL_WINDOW_S, T);
  if (stalls.length > 0)
    list.push({
      level: 'warn',
      code: 'video_stalled',
      since: iso(t0 + stalls[0].at * 1000),
      params: { count: stalls.length, seconds: r1(stalls.reduce((a, s) => a + s.seconds, 0)), window_s: STALL_WINDOW_S },
    });
  return list;
}

function media(): Media {
  if (role === 'rx') {
    const live = isLive();
    return { kind: 'output', label: 'OUT 1', format: live ? SIGNAL_4K30 : null, audio: 'none', black: !live };
  }
  // Names the operator recognises: the camera's name, the file name without its folder.
  const label =
    video.input.kind === 'camera'
      ? (OPTIONS.cameras.find((c) => c.device === video.input.camera)?.label ?? video.input.camera)
      : baseName(video.input.file);
  return { kind: video.input.kind, label, format: SIGNAL_4K30, audio: 'none', black: false };
}

function status(): Status {
  const t = now();
  const base = {
    server_time: iso(t),
    peer: peer(),
    listen_port: role === 'rx' ? 9000 : null,
    alerts: alerts(),
    media: media(),
    attempt: null,
    next_retry_s: null,
    last_live_until: null,
    live_s: null,
  };
  if (stoppedAt !== null) return { ...base, state: 'stopped', since: iso(stoppedAt) };
  if (scenario === 'error') {
    if (role === 'rx') return { ...base, state: 'listening', since: iso(lastLiveUntil), last_live_until: iso(lastLiveUntil) };
    return {
      ...base,
      state: 'reconnecting',
      since: iso(lastLiveUntil),
      attempt,
      next_retry_s: Math.max(0, Math.ceil((nextRetryAt - t) / 1000)),
      last_live_until: iso(lastLiveUntil),
    };
  }
  return { ...base, state: 'live', since: iso(liveSince), live_s: Math.floor((t - liveSince) / 1000) };
}

// ------------------------------------------------------------------ stats

const N = 60;
const METRICS: Metric[] = ['send_mbps', 'recv_mbps', 'target_mbps', 'actual_mbps', 'loss_pct', 'rtt_ms', 'bandwidth_mbps', 'delay_margin_s', 'stall_s'];

/** One history point: the bucket that ends at model time T (5 s). */
function bucket(T: number): Sample {
  const v = sample(T - 2.5);
  if (role === 'rx') v.stall_s = r1(stallsIn(T - 5, T).reduce((a, s) => a + s.seconds, 0));
  return v;
}

const point = (v: Sample, k: Metric) => (v[k] === undefined || v[k] === null ? null : r2(v[k]!));

function seedHistory() {
  // In the error scenario the link was lost 20 s ago: the last points are gaps.
  const lostAt = scenario === 'error' ? -20 : Infinity;
  history = Object.fromEntries(METRICS.map((k) => [k, [] as (number | null)[]])) as Record<Metric, (number | null)[]>;
  for (let i = 0; i < N; i++) {
    const T = -(N - 1 - i) * 5;
    const v = T <= lostAt ? bucket(T) : {};
    for (const k of METRICS) history[k].push(point(v, k));
  }
}

function pushHistory() {
  const v = isLive() ? bucket(modelT()) : {};
  for (const k of METRICS) {
    history[k].push(point(v, k));
    history[k].shift();
  }
}

function stats(): Stats {
  const live = isLive();
  const T = modelT();
  const v = sample(T);
  const profile = delayInEffect();
  const both = {
    at: iso(now()),
    delay_profile: profile,
    delay_target_ms: targetMsOf(profile),
    loss_pct: live ? r1(v.loss_pct!) : null,
    loss_threshold_pct: null, // standard profile: losses are recovered by resending
    rtt_ms: live ? Math.round(v.rtt_ms!) : null,
    last_rtt_ms: line(T).crowded ? 24 : 18,
  };
  if (role === 'tx')
    return {
      ...both,
      retrans_pct: null,
      send_mbps: live ? r1(v.send_mbps!) : null,
      target_mbps: live ? effectiveTarget(T) : null,
      actual_mbps: live ? r1(v.actual_mbps!) : null,
      bandwidth_mbps: live && v.bandwidth_mbps != null ? r1(v.bandwidth_mbps) : null,
      send_late_5m: live ? 0 : null,
      delay_margin_s: live ? (v.delay_margin_s ?? null) : null,
      delay_margin_warn_s: lowApplied() ? null : MARGIN_WARN_S,
      recv_mbps: null,
      stalls_5m: null,
      stall_s_5m: null,
      video_latency_ms: null,
      unrecovered_5m: null,
      display: null,
    };
  const stalls = stallsIn(T - STALL_WINDOW_S, T);
  return {
    ...both,
    retrans_pct: live ? r1(v.loss_pct! * 0.9) : null,
    send_mbps: null,
    target_mbps: null,
    actual_mbps: null,
    bandwidth_mbps: null,
    send_late_5m: null,
    delay_margin_s: null,
    delay_margin_warn_s: null,
    recv_mbps: live ? r1(v.recv_mbps!) : null,
    stalls_5m: live ? stalls.length : null,
    stall_s_5m: live ? r1(stalls.reduce((a, s) => a + s.seconds, 0)) : null,
    video_latency_ms: null, // not measured yet: the UI shows the profile target as 目安
    unrecovered_5m: live ? 0 : null,
    display: displayReport(),
  };
}

// ------------------------------------------------------------------ events

function ev(atMs: number, level: Level, code: EventCode, params?: Params): DeviceEvent {
  return { id: `e${++eventSeq}`, at: iso(atMs), level, code, ...(params ? { params } : {}) };
}

/** Events the model produced between model times `from` and `to` (bitrate steps, margin warnings, stalls). */
function modelEvents(from: number, to: number): DeviceEvent[] {
  const out: DeviceEvent[] = [];
  const at = (T: number) => t0 + T * 1000;
  if (role === 'tx') {
    let prev = line(from);
    let low = prev.margin < MARGIN_WARN_S;
    for (let T = from + 1; T <= to; T++) {
      const l = line(T);
      if (l.target !== prev.target) out.push(ev(at(T), 'info', 'bitrate_changed', { mbps: l.target, from_mbps: prev.target, reason: l.reason }));
      const nowLow = l.margin < MARGIN_WARN_S;
      if (nowLow && !low) out.push(ev(at(T), 'warn', 'delay_margin_low', { margin_s: marginAt(T) }));
      low = nowLow;
      prev = l;
    }
  } else {
    for (const s of stallsIn(from, to)) out.push(ev(at(s.at), 'warn', 'video_stalled', { seconds: s.seconds }));
  }
  return out;
}

function seedEvents() {
  const t = now();
  const ago = (h: number, m: number, s: number) => t - ((h * 60 + m) * 60 + s) * 1000;
  const lost = scenario === 'error';
  const list: DeviceEvent[] =
    role === 'tx'
      ? [
          ev(ago(0, 43, 37), 'info', 'input_detected', { format: SIGNAL_4K30 }),
          ev(ago(0, 42, 14), 'info', 'peer_connected'),
          ev(ago(0, 42, 13), 'info', 'transport_started'),
          ...modelEvents(-900, lost ? -20 : 0),
          ...(lost ? [ev(ago(0, 0, 20), 'warn', 'peer_disconnected')] : []),
        ]
      : [
          ev(ago(0, 47, 8), 'info', 'device_started'),
          ev(ago(0, 47, 6), 'info', 'listening_started', { port: 9000 }),
          ev(ago(0, 42, 14), 'info', 'peer_connected'),
          ev(ago(0, 42, 13), 'info', 'transport_started', { format: SIGNAL_4K30 }),
          ...modelEvents(-900, lost ? -20 : 0),
          ...(lost ? [ev(ago(0, 0, 20), 'warn', 'peer_disconnected'), ev(ago(0, 0, 19), 'info', 'listening_started', { port: 9000 })] : []),
        ];
  events = list.sort((a, b) => Date.parse(b.at) - Date.parse(a.at));
}

function addEvent(level: Level, code: EventCode, params?: Params) {
  pushEvent(ev(now(), level, code, params));
}
function pushEvent(e: DeviceEvent) {
  events.unshift(e);
  events = events.slice(0, 500);
  broadcast({ type: 'event', event: e });
}

// ------------------------------------------------------------------ settings logic

/** The input the draft selects differs from the applied one (only the field of the selected kind counts). */
const sourceChanged = (i: VideoSettingsInput['input']) =>
  i.kind !== video.input.kind || (i.kind === 'camera' ? i.camera !== video.input.camera : i.file !== video.input.file);

function checkVideo(input: VideoSettingsInput): CheckResult {
  const errors: FieldError[] = [];
  const impacts: Impact[] = [];
  const cur = effectiveTarget();
  const low = lowApplied();
  const br = input.encode.manual?.bitrate_mbps;
  const manual = input.encode.mode === 'manual';
  const [bMin, bMax] = OPTIONS.bitrate_range_mbps;
  const brValid = typeof br === 'number' && br >= bMin && br <= bMax;
  if (manual && !brValid) errors.push({ field: 'encode.manual.bitrate_mbps', code: 'out_of_range', params: { min: bMin, max: bMax } });
  if (input.input.kind === 'file' && input.input.loop && !OPTIONS.loop_available) errors.push({ field: 'input.loop', code: 'invalid' });

  const srcC = sourceChanged(input.input);
  const modeC = input.encode.mode !== video.encode.mode;
  const brC = manual && br !== cur;
  const loopC = input.input.kind === 'file' && input.input.loop !== video.input.loop;
  const any = srcC || modeC || brC || loopC;
  // While the delay is low the fixed bitrate is sent in either mode: a new mode alone changes nothing sent.
  const sentModeC = modeC && !low;

  const st = status().state;
  if (st === 'live') {
    if (srcC) {
      // A new input restarts the chain: the link is set up again (the UI shows the pair as one line).
      const gap_s = reconnectGapS(transport.delay.profile);
      impacts.push({ level: 'warn', code: 'input_switch_gap', params: { gap_s } });
      impacts.push({ level: 'warn', code: 'reconnect_gap', params: { gap_s } });
    } else if (sentModeC || brC) impacts.push({ level: 'ok', code: 'switch_no_gap' });
  } else if (st === 'stopped' && any) impacts.push({ level: 'info', code: 'saved_while_stopped' });
  if (modeC && manual) impacts.push({ level: 'info', code: 'manual_fixed' });
  if (sentModeC && !manual) impacts.push({ level: 'info', code: 'auto_decides' });
  // Not an error: the mode is stored and applies again when the delay is standard.
  if (low && input.encode.mode === 'auto')
    impacts.push({ level: 'info', code: 'low_delay_fixed', params: { bitrate_mbps: video.encode.manual.bitrate_mbps } });
  if (brC && brValid && br < cur) impacts.push({ level: 'info', code: 'bitrate_lower_quality' });
  const bw = stats().bandwidth_mbps;
  if (manual && (brC || modeC) && brValid && bw !== null && br > bw * 0.8)
    impacts.push({ level: 'warn', code: 'bitrate_near_bandwidth', params: { bandwidth_mbps: bw } });
  return { ok: errors.length === 0, errors, impacts };
}

const IPV4 = /^(25[0-5]|2[0-4]\d|1?\d?\d)(\.(25[0-5]|2[0-4]\d|1?\d?\d)){3}$/;
const portOk = (p: number) => Number.isInteger(p) && p >= 1 && p <= 65535;

/** The draft as the rules see it: the receiver follows the transmitter's delay, so its draft's delay is ignored. */
const asDevice = (input: TransportSettingsInput): TransportSettingsInput =>
  role === 'rx' ? { ...input, delay: { profile: transport.delay.profile } } : input;

/** The transport settings as served: tx the delay chosen here, rx the transmitter's it follows. */
const transportView = (): TransportSettings =>
  role === 'rx' ? { ...transport, delay: { profile: RX_PEER_DELAY, follows_peer: true } } : transport;

/** A field other than the send cap differs from the applied settings (applying would set the link up again). */
const linkFieldsChanged = (input: TransportSettingsInput) =>
  input.mode !== transport.mode ||
  input.peer?.host !== transport.peer.host ||
  input.peer?.port !== transport.peer.port ||
  input.listen_port !== transport.listen_port ||
  input.delay?.profile !== transport.delay.profile ||
  input.encryption?.cipher !== transport.encryption.cipher ||
  (input.encryption?.passphrase !== undefined && input.encryption.cipher !== 'none');

/** The cap a draft asks for: null when it is left out (kept) or not a number in range. */
function draftLimit(input: TransportSettingsInput): { given: boolean; mbps: number | null } {
  if (input.send_limit === undefined) return { given: false, mbps: null };
  const x = input.send_limit?.mbps;
  const [lo, hi] = SEND_LIMIT_RANGE;
  return { given: true, mbps: typeof x === 'number' && Number.isFinite(x) && x >= lo && x <= hi ? r2(x) : null };
}

function checkTransport(raw: TransportSettingsInput): CheckResult {
  const input = asDevice(raw);
  const errors: FieldError[] = [];
  const impacts: Impact[] = [];
  const limit = draftLimit(input);
  if (limit.given && limit.mbps === null)
    errors.push({ field: 'send_limit.mbps', code: 'out_of_range', params: { min: SEND_LIMIT_RANGE[0], max: SEND_LIMIT_RANGE[1] } });
  const limitChanged = limit.mbps !== null && limit.mbps !== transport.send_limit.mbps;
  const linkChanged = linkFieldsChanged(input);
  if (!TRANSPORT_OPTIONS.modes.includes(input.mode)) errors.push({ field: 'mode', code: 'invalid' });
  if (input.mode !== 'inbound') {
    if (!IPV4.test(input.peer.host)) errors.push({ field: 'peer.host', code: 'invalid' });
    if (!portOk(input.peer.port)) errors.push({ field: 'peer.port', code: 'out_of_range', params: { min: 1, max: 65535 } });
  }
  if (input.mode !== 'outbound' && !portOk(input.listen_port))
    errors.push({ field: 'listen_port', code: 'out_of_range', params: { min: 1, max: 65535 } });
  if (!TRANSPORT_OPTIONS.ciphers.includes(input.encryption.cipher)) errors.push({ field: 'encryption.cipher', code: 'invalid' });
  const pw = input.encryption.passphrase;
  if (pw !== undefined && input.encryption.cipher !== 'none' && (pw.length < 10 || pw.length > 79))
    errors.push({ field: 'encryption.passphrase', code: pw.length < 10 ? 'too_short' : 'too_long', params: { min: 10, max: 79 } });
  const dp = TRANSPORT_OPTIONS.delay_profiles.find((d) => d.profile === input.delay?.profile);
  if (!dp || !dp.available) errors.push({ field: 'delay.profile', code: 'invalid' });

  const st = status().state;
  if (st === 'stopped') impacts.push({ level: 'info', code: 'saved_while_stopped' });
  // The cap alone sets nothing up again: it is read when a transmission starts (retries and
  // reconnects keep the cap they started with), so it is stored now and used from the next start.
  else if (limitChanged && !linkChanged) impacts.push({ level: 'info', code: 'takes_effect_next_start' });
  else if (st === 'live')
    // After the reconnect the receiver's picture comes back after about the delay of the new profile.
    impacts.push({ level: 'warn', code: 'reconnect_gap', params: { gap_s: reconnectGapS(dp?.available ? dp.profile : transport.delay.profile) } });
  else {
    impacts.push({ level: 'info', code: 'reconnect' });
    impacts.push({ level: 'ok', code: 'no_peer_effect' });
  }
  // To low delay: automatic mode is not used while it is applied (the stored video mode is kept).
  if (dp?.available && dp.profile === 'low' && transport.delay.profile !== 'low') {
    if (video.encode.mode === 'auto')
      impacts.push({ level: 'info', code: 'low_delay_fixed', params: { bitrate_mbps: video.encode.manual.bitrate_mbps } });
    impacts.push({ level: 'info', code: 'low_delay_effects' });
  }
  return { ok: errors.length === 0, errors, impacts };
}

function compare(): CompareResult {
  const known = negotiatedAt > 0;
  const cipher = transport.encryption.cipher;
  return {
    received_at: known ? iso(negotiatedAt) : null,
    peer_id: 'RX-02',
    rows: [
      { key: 'mode', local: transport.mode, peer: 'inbound', verdict: transport.mode === 'inbound' ? 'incompatible' : 'compatible' },
      {
        key: 'endpoint',
        local: { ...transport.peer },
        peer: { listen_port: 9000 },
        verdict: transport.peer.port === 9000 ? 'match' : 'mismatch',
      },
      // the receiver follows the transmitter's delay when it connects
      { key: 'delay', local: transport.delay.profile, peer: transport.delay.profile, verdict: 'match' },
      { key: 'cipher', local: cipher, peer: PEER_CIPHER, verdict: cipher === PEER_CIPHER ? 'match' : 'mismatch' },
      {
        key: 'passphrase',
        local_set: transport.encryption.passphrase_set,
        verdict: cipher === 'none' || stoppedAt !== null || scenario === 'error' ? 'unknown' : 'match',
        reason: null,
      },
    ],
  };
}

// ------------------------------------------------------------------ network settings (途中の無線機)

const HOPS_MAX = 4;
const HOP_LABEL_MAX = 24;
/** A dotted quad without leading zeros (as the device parses it). */
const DOTTED_QUAD = /^(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)(\.(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)){3}$/;
const usableHopAddress = (a: string) =>
  DOTTED_QUAD.test(a) && a !== '0.0.0.0' && a !== '255.255.255.255' && !a.startsWith('127.');

/** The device's configured list: the radio next to this device first, then the one next to the peer. */
const defaultHops = (): Hop[] =>
  role === 'tx'
    ? [
        { label: '無線機（送信機側）', address: RADIO_TX_SIDE },
        { label: '無線機（受信機側）', address: RADIO_RX_SIDE },
      ]
    : [
        { label: '無線機（受信機側）', address: RADIO_RX_SIDE },
        { label: '無線機（送信機側）', address: RADIO_TX_SIDE },
      ];

const sameHops = (a: Hop[], b: Hop[]) =>
  a.length === b.length && a.every((h, i) => h.label === b[i].label && h.address === b[i].address);

/**
 * A label trimmed as the device trims it (Unicode White_Space): like String.prototype.trim,
 * except that U+0085 is trimmed and U+FEFF is not.
 */
const WS = '\\t\\n\\v\\f\\r \\u0085\\u00a0\\u1680\\u2000-\\u200a\\u2028\\u2029\\u202f\\u205f\\u3000';
const EDGE_WS = new RegExp(`^[${WS}]+|[${WS}]+$`, 'g');
const trimLabel = (s: string) => s.replace(EDGE_WS, '');

/** The list as the device would store it (labels trimmed), or null when the body has no list. */
function hopsOf(input: Partial<NetworkSettingsInput>): Hop[] | null {
  if (!Array.isArray(input.hops)) return null;
  return input.hops.map((h) => ({
    label: typeof h?.label === 'string' ? trimLabel(h.label) : '',
    address: typeof h?.address === 'string' ? h.address : '',
  }));
}

const readNetworkInput = (req: IncomingMessage) => readObject<NetworkSettingsInput>(req);

/**
 * The body as the device reads it: empty → {}; not JSON → null (400 invalid without field
 * errors); JSON that is not an object (null, a string, a list) → {} (its fields are missing).
 */
async function readObject<T>(req: IncomingMessage): Promise<Partial<T> | null> {
  const chunks: Buffer[] = [];
  for await (const c of req) chunks.push(c as Buffer);
  const text = Buffer.concat(chunks).toString('utf8');
  if (/^[ \t\n\f\r]*$/.test(text)) return {};
  try {
    const v: unknown = JSON.parse(text);
    return v !== null && typeof v === 'object' && !Array.isArray(v) ? (v as Partial<T>) : {};
  } catch {
    return null;
  }
}

/** The device's rules: one code per field, the address codes in the documented order. */
function checkNetwork(input: Partial<NetworkSettingsInput>): CheckResult {
  const errors: FieldError[] = [];
  const impacts: Impact[] = [];
  const hops = hopsOf(input);
  if (hops === null) return { ok: false, errors: [{ field: 'hops', code: 'required' }], impacts };
  if (hops.length > HOPS_MAX) errors.push({ field: 'hops', code: 'too_many', params: { max: HOPS_MAX } });
  const self = device().address;
  const peerAddress = peer().address;
  const seen = new Set<string>();
  hops.forEach((h, i) => {
    const n = [...h.label].length;
    if (n === 0) errors.push({ field: `hops[${i}].label`, code: 'required' });
    else if (n > HOP_LABEL_MAX) errors.push({ field: `hops[${i}].label`, code: 'too_long', params: { max: HOP_LABEL_MAX } });
    const field = `hops[${i}].address`;
    if (!usableHopAddress(h.address)) errors.push({ field, code: 'invalid' });
    else if (h.address === self) errors.push({ field, code: 'is_self' });
    else if (h.address === peerAddress) errors.push({ field, code: 'is_peer' });
    else if (seen.has(h.address)) errors.push({ field, code: 'duplicate' });
    seen.add(h.address);
  });
  if (!sameHops(hops, network.hops)) impacts.push({ level: 'info', code: 'hops_changed' });
  return { ok: errors.length === 0, errors, impacts };
}

// ------------------------------------------------------------------ 受信機のモニター (output settings, rx)

const DISPLAYS: OutputDisplay[] = ['off', 'fullscreen', 'window'];
const DISPLAY_FAILS = process.env.MOCK_DISPLAY === 'failed';
const displayOf = (v: unknown): OutputDisplay | null =>
  typeof v === 'string' && (DISPLAYS as string[]).includes(v) ? (v as OutputDisplay) : null;

/** What the receiver reports about its monitor: nothing while stopped (no player runs). */
function displayReport(): Stats['display'] {
  if (role !== 'rx' || stoppedAt !== null) return null;
  if (output.display === 'off') return 'off';
  return DISPLAY_FAILS ? 'failed' : 'on';
}

/** The device's rules: one field; impacts only when the value changes. */
function checkOutput(input: Partial<OutputSettingsInput>): CheckResult {
  const to = displayOf(input.display);
  if (to === null) return { ok: false, errors: [{ field: 'display', code: 'invalid' }], impacts: [] };
  const impacts: Impact[] = [];
  if (to !== output.display) {
    impacts.push({ level: 'info', code: to === 'off' ? 'display_off' : 'display_on' });
    if (to === 'fullscreen') impacts.push({ level: 'info', code: 'display_fullscreen_covers' });
  }
  return { ok: true, errors: [], impacts };
}

/** Store a new monitor output as the device does (the version goes up also for the same value). */
function setOutputDisplay(d: OutputDisplay): OutputSettings {
  output = { version: output.version + 1, display: d };
  addEvent('info', 'output_settings_applied', { display: d });
  broadcast({ type: 'settings', section: 'output', version: output.version });
  return output;
}

// ------------------------------------------------------------------ network test

/**
 * The line of the real installation: a radio link with a round trip of about 75 ms, a radio next
 * to each device. tx: the near radio is on the transmitter side, rx the other way round.
 * つながりの確認 checks the applied 途中の無線機; an address where neither radio is does not answer.
 * The line-speed test sends saturate_mbps (6) and measures what arrives, as on the device:
 *   normal  everything answers; towards the receiver 5.26 Mbps arrive (5.8 % lost, the line is
 *           full), back 1.15 Mbps
 *   warn    the radio link is worse: an answer lost now and then, 3.18 and 0.86 Mbps
 *   error   the radio link is down: the near radio answers, the far one and the peer do not;
 *           line-speed tests fail after the 3 s connection timeout with peer_unreachable
 *   MOCK_LINE_FAST=1  the line takes everything the test sends (not saturated: 「6 Mbps 以上」)
 * Line-speed tests run only while stopped (409 busy otherwise) and block /transport/start, as on
 * the device. While one runs only `progress` moves; the result comes at the end.
 */
const NETWORK_TEST_OPTIONS: NetworkTestOptions = { duration_range_s: [5, 30], saturate_mbps: 6 };
const LINE_FAST = process.env.MOCK_LINE_FAST === '1';
const REACH_MS = 2400; // 10 answers 0.2 s apart, the last one waited for up to 1 s
const CONNECT_TIMEOUT_MS = 3000;
const HISTORY_MAX = 20;
const RADIO_TX_SIDE = '10.20.1.65';
const RADIO_RX_SIDE = '10.20.1.56';

interface MockTest {
  test: NetworkTest;
  startMs: number;
  durMs: number;
  finish: () => Pick<NetworkTest, 'result' | 'error'>;
}
let tests: MockTest[] = [];
let testSeq = 0;

const wave = (i: number, a: number) => a * (0.6 * Math.sin(i * 1.7 + 0.4) + 0.4 * Math.sin(i * 0.61 + 2.1));
const mean = (v: number[]) => (v.length ? v.reduce((a, b) => a + b, 0) / v.length : 0);

/** One reachability target: `rtt` the typical round trip, `got` how many of 10 answers come back. */
function target(kind: 'peer' | 'hop', label: string | null, address: string, rtt: number, got: number): ReachabilityTarget {
  const seen = got > 0;
  const k = rtt < 5 ? 0.15 : 0.06;
  return {
    kind,
    label,
    address,
    sent: 10,
    received: got,
    rtt_min_ms: seen ? r2(rtt * (1 - k)) : null,
    rtt_avg_ms: seen ? r2(rtt) : null,
    rtt_max_ms: seen ? r2(rtt * (1 + 2 * k)) : null,
  };
}

function reachTargets(): ReachabilityTarget[] {
  const down = scenario === 'error';
  const lossy = scenario === 'warn';
  const far = down ? 0 : lossy ? 9 : 10;
  const near = role === 'tx' ? RADIO_TX_SIDE : RADIO_RX_SIDE;
  const other = role === 'tx' ? RADIO_RX_SIDE : RADIO_TX_SIDE;
  const hop = (h: Hop) =>
    h.address === near
      ? target('hop', h.label, h.address, 0.42, 10)
      : h.address === other
        ? target('hop', h.label, h.address, lossy ? 77.9 : 74.8, far)
        : target('hop', h.label, h.address, 1, 0); // nothing there
  return [target('peer', null, peer().address, lossy ? 78.6 : 75.4, far), ...network.hops.map(hop)];
}

/** What arrives when the test sends saturate_mbps (tx view: to_peer = towards the receiver). */
function arrival(direction: NetworkTestDirection): { mbps: number; loss: number; jitter: number } {
  const toRx = direction === 'to_peer';
  if (LINE_FAST) return toRx ? { mbps: 5.98, loss: 0.1, jitter: 0.8 } : { mbps: 5.97, loss: 0.2, jitter: 1.1 };
  if (scenario === 'warn') return toRx ? { mbps: 3.18, loss: 46.8, jitter: 6.2 } : { mbps: 0.86, loss: 85.6, jitter: 12.5 };
  return toRx ? { mbps: 5.26, loss: 5.8, jitter: 3.1 } : { mbps: 1.15, loss: 80.8, jitter: 9.4 };
}

function planThroughput(p: NetworkTestParams): Pick<MockTest, 'durMs' | 'finish'> {
  const dur = p.duration_s ?? 10;
  if (scenario === 'error') return { durMs: CONNECT_TIMEOUT_MS, finish: () => ({ result: null, error: 'peer_unreachable' }) };
  const a = arrival(p.direction ?? 'to_peer');
  // What arrives in each second wobbles around the line's pace; scaled so that the mean is the result.
  const raw = Array.from({ length: dur }, (_, i) => 1 + wave(i, a.loss >= 1 ? 0.04 : 0.003));
  const seconds = raw.map((v) => r2((v * a.mbps) / mean(raw)));
  return {
    durMs: dur * 1000,
    finish: () => ({
      result: {
        throughput_mbps: a.mbps,
        sent_mbps: r2(NETWORK_TEST_OPTIONS.saturate_mbps * 1.0004),
        loss_pct: a.loss,
        jitter_ms: a.jitter,
        saturated: a.loss >= 1,
        per_second_mbps: seconds,
      },
      error: null,
    }),
  };
}

/** Bring a test up to now: progress, the end. */
function advance(m: MockTest) {
  const t = m.test;
  if (t.state !== 'running') return;
  const elapsed = now() - m.startMs;
  if (elapsed < m.durMs) {
    // A line-speed test that cannot reach the peer has nothing to show until it gives up.
    if (t.kind === 'reachability' || scenario !== 'error') t.progress = r2(elapsed / m.durMs);
    return;
  }
  const { result, error } = m.finish();
  Object.assign(t, { state: error ? 'failed' : 'done', finished_at: iso(m.startMs + m.durMs), result, error });
  if (!error) t.progress = 1;
  if (t.kind === 'throughput' && t.result)
    addEvent('info', 'network_test_done', { mbps: r2(t.result.throughput_mbps), direction: t.request.direction ?? 'to_peer' });
}
const advanceTests = () => tests.forEach(advance);
const throughputRunning = () => tests.some((m) => m.test.kind === 'throughput' && m.test.state === 'running');

const DIRECTIONS: NetworkTestDirection[] = ['to_peer', 'from_peer'];

/** POST /network-tests, with the device's order of checks. mode and rate_mbps of older clients are ignored. */
function startNetworkTest(b: Partial<NetworkTestRequest>): { code: number; body: NetworkTest | ApiError } {
  const invalid = (errors: FieldError[]) => ({ code: 400, body: { error: { code: 'invalid' as const, errors } } });
  if (b.kind !== 'reachability' && b.kind !== 'throughput')
    return invalid([{ field: 'kind', code: b.kind === undefined ? 'required' : 'invalid' }]);
  if (b.kind === 'throughput' && role !== 'tx') return { code: 409, body: { error: { code: 'unsupported_role' } } };

  let params: NetworkTestParams = { kind: 'reachability', direction: null, duration_s: null };
  if (b.kind === 'throughput') {
    const errors: FieldError[] = [];
    const direction = b.direction ?? 'to_peer';
    const [dMin, dMax] = NETWORK_TEST_OPTIONS.duration_range_s;
    const duration = b.duration_s ?? 10;
    if (!DIRECTIONS.includes(direction)) errors.push({ field: 'direction', code: 'invalid' });
    if (!(Number.isInteger(duration) && duration >= dMin && duration <= dMax))
      errors.push({ field: 'duration_s', code: 'out_of_range', params: { min: dMin, max: dMax } });
    if (errors.length) return invalid(errors);
    if (status().state !== 'stopped' || throughputRunning()) return { code: 409, body: { error: { code: 'busy' } } };
    params = { kind: 'throughput', direction, duration_s: duration };
  } else if (tests.some((m) => m.test.kind === 'reachability' && m.test.state === 'running')) {
    return { code: 409, body: { error: { code: 'busy' } } };
  }

  const startMs = now();
  const base = { id: `nt-${++testSeq}`, state: 'running' as const, started_at: iso(startMs), finished_at: null, progress: 0, error: null };
  let entry: MockTest;
  if (params.kind === 'reachability') {
    const targets = reachTargets();
    entry = {
      test: { ...base, kind: 'reachability', request: params, result: null },
      startMs,
      durMs: REACH_MS,
      finish: () => ({ result: { targets }, error: null }),
    };
  } else {
    entry = { test: { ...base, kind: 'throughput', request: params, result: null }, startMs, ...planThroughput(params) };
  }
  tests = [entry, ...tests].slice(0, HISTORY_MAX);
  return { code: 202, body: entry.test };
}

/**
 * POST /network-tests/{id}/use: a finished line-speed test towards the receiver becomes the send
 * cap (what arrived, or saturate_mbps when the line took everything; kept within the range).
 * The next start uses it, also when used while live.
 */
function useNetworkTest(id: string): { code: number; body: TransportSettings | ApiError } {
  if (role !== 'tx') return { code: 409, body: { error: { code: 'unsupported_role' } } };
  const t = testById(id);
  if (!t) return { code: 404, body: { error: { code: 'not_found' } } };
  if (t.kind !== 'throughput' || t.state !== 'done' || !t.result || t.request.direction !== 'to_peer')
    return { code: 409, body: { error: { code: 'not_applicable' } } };
  const [lo, hi] = SEND_LIMIT_RANGE;
  const measured = t.result.saturated ? t.result.throughput_mbps : NETWORK_TEST_OPTIONS.saturate_mbps;
  const mbps = r2(Math.min(hi, Math.max(lo, measured)));
  const from = transport.send_limit.mbps;
  transport = {
    ...transport,
    version: transport.version + 1,
    send_limit: { mbps, source: 'measured', measured_at: t.finished_at, test_id: t.id },
  };
  addEvent('info', 'send_limit_changed', { mbps, from_mbps: from, source: 'measured' });
  broadcast({ type: 'settings', section: 'transport', version: transport.version });
  return { code: 200, body: transportView() };
}

const testList = (): NetworkTestList => {
  advanceTests();
  return { items: tests.map((m) => m.test) };
};
const testById = (id: string): NetworkTest | null => {
  advanceTests();
  return tests.find((m) => m.test.id === id)?.test ?? null;
};

// ------------------------------------------------------------------ HTTP

function send(res: ServerResponse, code: number, body?: unknown) {
  res.writeHead(code, { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store' });
  res.end(body === undefined ? '' : JSON.stringify(body));
}

async function readJson<T>(req: IncomingMessage): Promise<T> {
  const chunks: Buffer[] = [];
  for await (const c of req) chunks.push(c as Buffer);
  return JSON.parse(Buffer.concat(chunks).toString('utf8') || '{}') as T;
}

const fail = (res: ServerResponse, code: number, name: ApiError['error']['code']) => send(res, code, { error: { code: name } } satisfies ApiError);

/**
 * Preview sessions. The mock cannot stream a picture, so a well-formed offer while live is
 * answered 503 preview_unavailable (the screens then show 「プレビューを表示できませんでした」).
 */
const PREVIEW_OFFER_MAX = 64 * 1024; // bytes of the offer text, as the daemon counts them
async function startPreview(req: IncomingMessage, res: ServerResponse) {
  // Same order of checks as the device: the offer first, then the link state.
  let offer: unknown;
  try {
    offer = (await readJson<Partial<PreviewOffer>>(req)).offer;
  } catch {
    offer = undefined;
  }
  if (typeof offer !== 'string' || !offer.trimStart().startsWith('v=0') || Buffer.byteLength(offer) > PREVIEW_OFFER_MAX) {
    return fail(res, 400, 'bad_offer');
  }
  if (!isLive()) return fail(res, 409, 'not_live');
  return fail(res, 503, 'preview_unavailable');
}

const videoView = (): VideoSettings => ({ ...video, encode: { ...video.encode, auto: { bitrate_mbps: autoTarget() } } });

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url ?? '/', 'http://localhost');
  const p = url.pathname;
  const m = req.method ?? 'GET';
  try {
    // development switches
    if (m === 'POST' && p === '/mock/role') {
      const b = await readJson<{ role: Role }>(req);
      role = b.role === 'rx' ? 'rx' : 'tx';
      reset();
      broadcastHello();
      return send(res, 200, { role, scenario });
    }
    if (m === 'POST' && p === '/mock/scenario') {
      const b = await readJson<{ scenario: Scenario }>(req);
      scenario = (['normal', 'warn', 'error'] as const).includes(b.scenario) ? b.scenario : scenario;
      reset();
      broadcastHello();
      return send(res, 200, { role, scenario });
    }

    if (!p.startsWith('/api/v1/')) return send(res, 404, { error: { code: 'not_found' } });
    const path = p.slice('/api/v1'.length);

    if (m === 'GET' && path === '/device') return send(res, 200, device());
    if (m === 'GET' && path === '/status') return send(res, 200, status());
    if (m === 'GET' && path === '/stats') return send(res, 200, stats());
    if (m === 'GET' && path === '/stats/history') {
      const h: StatsHistory = { interval_s: 5, end: iso(now()), series: { ...history } };
      return send(res, 200, h);
    }
    if (m === 'GET' && path === '/events') {
      const limit = Math.min(500, Number(url.searchParams.get('limit') ?? 20));
      return send(res, 200, events.slice(0, limit));
    }
    if (m === 'POST' && path === '/preview/sessions') return startPreview(req, res);
    if (m === 'DELETE' && /^\/preview\/sessions\/[^/]+$/.test(path)) return send(res, 204);

    if (m === 'POST' && path === '/transport/stop') {
      if (stoppedAt === null) {
        stoppedAt = now();
        addEvent('info', 'transport_stopped');
      }
      broadcast({ type: 'status', status: status() });
      return send(res, 200, status());
    }
    if (m === 'POST' && path === '/transport/start') {
      // The line is being measured: starting now would spoil both.
      if (throughputRunning()) return send(res, 409, { error: { code: 'busy' } });
      if (stoppedAt !== null) {
        stoppedAt = null;
        liveSince = now();
        nextRetryAt = now() + 5000;
        lastAutoTarget = autoTarget();
        addEvent('info', 'transport_started', role === 'rx' ? { format: SIGNAL_4K30 } : undefined);
      }
      broadcast({ type: 'status', status: status() });
      return send(res, 200, status());
    }
    if (m === 'POST' && path === '/transport/retry') {
      if (scenario === 'error' && stoppedAt === null && role === 'tx') {
        attempt += 1;
        nextRetryAt = now() + 5000;
      }
      broadcast({ type: 'status', status: status() });
      return send(res, 200, status());
    }

    // video (transmitter)
    if (path.startsWith('/settings/video') && role !== 'tx') return send(res, 409, { error: { code: 'unsupported_role' } });
    if (m === 'GET' && path === '/settings/video') return send(res, 200, videoView());
    if (m === 'GET' && path === '/settings/video/options') {
      const o: VideoOptions = { ...OPTIONS, auto_available: !lowApplied(), bandwidth_mbps: stats().bandwidth_mbps };
      return send(res, 200, o);
    }
    if (m === 'POST' && path === '/settings/video/check') return send(res, 200, checkVideo(await readJson<VideoSettingsInput>(req)));
    if (m === 'PUT' && path === '/settings/video') {
      const input = await readJson<VideoSettingsInput>(req);
      if (input.version !== video.version) return send(res, 409, { error: { code: 'version_conflict' } });
      const c = checkVideo(input);
      if (!c.ok) return send(res, 400, { error: { code: 'invalid', errors: c.errors } });
      const srcChanged = sourceChanged(input.input);
      await sleep(1500); // the device switches over (bitrate: a few seconds, no gap)
      const prevBr = effectiveTarget();
      const from = rampedTarget(modelT());
      video = {
        ...video,
        version: video.version + 1,
        input: { ...input.input },
        encode: {
          mode: input.encode.mode,
          manual: input.encode.mode === 'manual' ? { bitrate_mbps: input.encode.manual.bitrate_mbps } : video.encode.manual,
          auto: video.encode.auto,
        },
      };
      lastAutoTarget = autoTarget();
      addEvent('info', 'video_settings_applied');
      if (srcChanged) addEvent('info', 'input_detected', { format: SIGNAL_4K30 });
      const nextBr = effectiveTarget();
      if (nextBr !== prevBr) {
        switchRamp = { at: modelT(), from };
        addEvent('info', 'bitrate_changed', { mbps: nextBr, from_mbps: prevBr, reason: 'manual' });
      }
      broadcast({ type: 'settings', section: 'video', version: video.version });
      broadcast({ type: 'status', status: status() });
      const out: ApplyResult<VideoSettings> = { settings: videoView(), applied_at: iso(now()), impacts: c.impacts };
      return send(res, 200, out);
    }

    // transport (both roles; the comparison is the transmitter's)
    if (path === '/transport/compare' && role !== 'tx') return send(res, 409, { error: { code: 'unsupported_role' } });
    if (m === 'GET' && path === '/settings/transport') return send(res, 200, transportView());
    if (m === 'GET' && path === '/settings/transport/options') return send(res, 200, TRANSPORT_OPTIONS);
    if (m === 'GET' && path === '/transport/compare') return send(res, 200, compare());
    if (m === 'POST' && path === '/settings/transport/check') return send(res, 200, checkTransport(await readJson<TransportSettingsInput>(req)));
    if (m === 'PUT' && path === '/settings/transport') {
      const input = asDevice(await readJson<TransportSettingsInput>(req));
      if (input.version !== transport.version) return send(res, 409, { error: { code: 'version_conflict' } });
      const c = checkTransport(input);
      if (!c.ok) return send(res, 400, { error: { code: 'invalid', errors: c.errors } });
      const prevLimit = transport.send_limit;
      const nextMbps = draftLimit(input).mbps ?? prevLimit.mbps;
      const limitChanged = nextMbps !== prevLimit.mbps;
      const linkChanged = linkFieldsChanged(input);
      // Setting the link up again takes a moment; storing the cap alone does not.
      await sleep(linkChanged || !limitChanged ? 1000 : 300);
      const newPass = input.encryption.passphrase !== undefined && input.encryption.cipher !== 'none';
      const prevBr = effectiveTarget();
      const from = rampedTarget(modelT());
      transport = {
        ...transport,
        version: transport.version + 1,
        mode: input.mode,
        peer: { ...input.peer },
        listen_port: input.listen_port,
        delay: { profile: input.delay.profile, follows_peer: false },
        encryption: {
          cipher: input.encryption.cipher,
          passphrase_set: input.encryption.cipher === 'none' ? transport.encryption.passphrase_set : transport.encryption.passphrase_set || newPass,
          passphrase_rejected_at: newPass ? null : transport.encryption.passphrase_rejected_at,
        },
        // A new value is the operator's; the same value keeps where it came from.
        send_limit: limitChanged ? { mbps: nextMbps, source: 'manual', measured_at: null, test_id: null } : prevLimit,
      };
      // a new delay can change the bitrate in effect (low delay sends the fixed one)
      if (effectiveTarget() !== prevBr) {
        switchRamp = { at: modelT(), from };
        lastAutoTarget = autoTarget();
      }
      if (linkChanged || !limitChanged) addEvent('info', 'transport_settings_applied');
      if (newPass) addEvent('info', 'passphrase_changed');
      if (limitChanged) addEvent('info', 'send_limit_changed', { mbps: nextMbps, from_mbps: prevLimit.mbps, source: 'manual' });
      broadcast({ type: 'settings', section: 'transport', version: transport.version });
      broadcast({ type: 'status', status: status() });
      const out: ApplyResult<TransportSettings> = { settings: transportView(), applied_at: iso(now()), impacts: c.impacts };
      return send(res, 200, out);
    }

    // 途中の無線機 (both roles; allowed while live: the transmission is not touched)
    if (m === 'GET' && path === '/settings/network') return send(res, 200, network);
    if (m === 'POST' && path === '/settings/network/check') {
      const input = await readNetworkInput(req);
      if (!input) return send(res, 400, { error: { code: 'invalid', errors: [] } });
      return send(res, 200, checkNetwork(input));
    }
    if (m === 'PUT' && path === '/settings/network') {
      const input = await readNetworkInput(req);
      if (!input) return send(res, 400, { error: { code: 'invalid', errors: [] } });
      if (input.version !== network.version) return send(res, 409, { error: { code: 'version_conflict' } });
      const c = checkNetwork(input);
      if (!c.ok) return send(res, 400, { error: { code: 'invalid', errors: c.errors } });
      await sleep(300); // stored; what the radio line may carry follows the new list
      network = { version: network.version + 1, hops: hopsOf(input)! };
      addEvent('info', 'network_settings_applied');
      broadcast({ type: 'settings', section: 'network', version: network.version });
      return send(res, 200, network);
    }

    // 受信機のモニター (the receiver's; allowed in every state: the picture follows at once)
    if ((path === '/settings/output' || path.startsWith('/settings/output/') || path === '/display') && role !== 'rx')
      return send(res, 409, { error: { code: 'unsupported_role' } });
    if (m === 'GET' && path === '/settings/output') return send(res, 200, output);
    if (m === 'POST' && path === '/settings/output/check') {
      const input = await readObject<OutputSettingsInput>(req);
      if (!input) return send(res, 400, { error: { code: 'invalid', errors: [] } });
      return send(res, 200, checkOutput(input));
    }
    if (m === 'PUT' && path === '/settings/output') {
      const input = await readObject<OutputSettingsInput>(req);
      if (!input) return send(res, 400, { error: { code: 'invalid', errors: [] } });
      if (input.version !== output.version) return send(res, 409, { error: { code: 'version_conflict' } });
      const c = checkOutput(input);
      if (!c.ok) return send(res, 400, { error: { code: 'invalid', errors: c.errors } });
      await sleep(200); // the receiver is given the new value
      return send(res, 200, setOutputDisplay(displayOf(input.display)!));
    }
    // the operation screen's 低遅延表示: no version, no draft
    if (m === 'POST' && path === '/display') {
      const input = await readObject<DisplayRequest>(req);
      if (!input) return send(res, 400, { error: { code: 'invalid', errors: [] } });
      const mode = displayOf(input.mode);
      if (mode === null) return send(res, 400, { error: { code: 'invalid', errors: [{ field: 'mode', code: 'invalid' }] } });
      await sleep(200);
      return send(res, 200, setOutputDisplay(mode));
    }

    // network test
    if (m === 'POST' && path === '/network-tests') {
      let b: Partial<NetworkTestRequest>;
      try {
        b = await readJson<Partial<NetworkTestRequest>>(req);
      } catch {
        b = {};
      }
      const out = startNetworkTest(b);
      return send(res, out.code, out.body);
    }
    if (m === 'GET' && path === '/network-tests') return send(res, 200, testList());
    if (m === 'GET' && path === '/network-tests/options') return send(res, 200, NETWORK_TEST_OPTIONS);
    const use = path.match(/^\/network-tests\/([\w-]+)\/use$/);
    if (m === 'POST' && use) {
      const out = useNetworkTest(use[1]);
      return send(res, out.code, out.body);
    }
    const nt = path.match(/^\/network-tests\/([\w-]+)$/);
    if (m === 'GET' && nt) {
      const t = testById(nt[1]);
      return t ? send(res, 200, t) : send(res, 404, { error: { code: 'not_found' } });
    }

    return send(res, 404, { error: { code: 'not_found' } });
  } catch (e) {
    console.error(e);
    return send(res, 500, { error: { code: 'invalid' } });
  }
});

// ------------------------------------------------------------------ live channel

const wss = new WebSocketServer({ noServer: true });
server.on('upgrade', (req, socket, head) => {
  if (new URL(req.url ?? '/', 'http://localhost').pathname !== '/api/v1/live') return socket.destroy();
  wss.handleUpgrade(req, socket, head, (ws) => {
    ws.send(JSON.stringify(hello()));
  });
});

const hello = (): LiveMessage => ({ type: 'hello', device: device(), status: status(), stats: stats() });

function broadcast(msg: LiveMessage) {
  const s = JSON.stringify(msg);
  for (const c of wss.clients) if (c.readyState === WebSocket.OPEN) c.send(s);
}
const broadcastHello = () => broadcast(hello());

setInterval(() => {
  tick += 1;
  advanceTests(); // finishes tests nobody is polling, so their events still appear
  const T = modelT();
  if (scenario === 'error' && stoppedAt === null && role === 'tx' && now() >= nextRetryAt) {
    attempt += 1;
    nextRetryAt = now() + 5000;
  }
  if (isLive()) {
    if (role === 'tx') {
      // Automatic mode steps the bitrate as the line changes.
      const l = line(T);
      if (autoInEffect() && lastAutoTarget !== null && l.target !== lastAutoTarget)
        addEvent('info', 'bitrate_changed', { mbps: l.target, from_mbps: lastAutoTarget, reason: l.reason });
      lastAutoTarget = l.target;
      const low = marginLowSince(T) !== null;
      if (low && !wasMarginLow) addEvent('warn', 'delay_margin_low', { margin_s: marginAt(T) });
      wasMarginLow = low;
    } else {
      for (const s of stallsIn(lastTickT, T)) addEvent('warn', 'video_stalled', { seconds: s.seconds });
    }
  }
  lastTickT = T;
  if (tick % 5 === 0) pushHistory();
  broadcast({ type: 'stats', stats: stats() });
  broadcast({ type: 'status', status: status() });
}, 1000);

reset();
server.listen(PORT, () => {
  console.log(`mock device API: http://127.0.0.1:${PORT}/api/v1  role=${role} scenario=${scenario}`);
});
