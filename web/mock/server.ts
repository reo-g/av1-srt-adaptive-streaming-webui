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
 *
 * Development-only endpoints: POST /mock/role {role}, POST /mock/scenario {scenario}
 */
import { readdirSync, readFileSync } from 'node:fs';
import http, { type IncomingMessage, type ServerResponse } from 'node:http';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { WebSocket, WebSocketServer } from 'ws';
import type {
  Alert,
  ApplyResult,
  CheckResult,
  CompareResult,
  Device,
  DeviceEvent,
  EventCode,
  FieldError,
  Impact,
  Level,
  LiveMessage,
  Media,
  Metric,
  NetworkTest,
  Params,
  Role,
  Stats,
  StatsHistory,
  Status,
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

const here = dirname(fileURLToPath(import.meta.url));
const frames = readdirSync(join(here, 'assets'))
  .filter((f) => f.endsWith('.jpg'))
  .sort()
  .map((f) => readFileSync(join(here, 'assets', f)));

// ------------------------------------------------------------------ time helpers

/** ISO 8601 in JST with offset, without milliseconds. */
const iso = (ms: number) => new Date(ms + 9 * 3600e3).toISOString().replace(/\.\d{3}Z$/, '+09:00');
const now = () => Date.now();
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

// ------------------------------------------------------------------ state

let liveSince = 0;
let stoppedAt: number | null = null;
let attempt = 5;
let nextRetryAt = 0;
let lastLiveUntil = 0;
let rejectedAt = 0;
let negotiatedAt = 0;
let events: DeviceEvent[] = [];
let eventSeq = 0;
let history: Record<Metric, (number | null)[]>;
let tick = 0;

const device = (): Device =>
  role === 'tx'
    ? { id: 'TX-01', role, address: '192.168.10.21', firmware: '1.0.0', serial: 'VT-2026-000123', timezone: 'Asia/Tokyo' }
    : { id: 'RX-02', role, address: '192.168.10.22', firmware: '1.0.0', serial: 'VR-2026-000456', timezone: 'Asia/Tokyo' };

const peer = () =>
  role === 'tx' ? { id: 'RX-02', address: transport.peer.host, port: transport.peer.port } : { id: 'TX-01', address: '10.20.1.21', port: 9000 };

const ENC = {
  resolution: 'source',
  framerate: 'source',
  bitrate_mbps: 12,
  rate_control: 'cbr',
  codec: 'h264',
  profile: 'high',
  keyframe_s: 1,
  bframes: 0,
  encoder: 'hardware',
} as const;

let video: VideoSettings;
let transport: TransportSettings;

const PEER_BUFFER_MS = 120;
const PEER_CIPHER = 'aes-128';

const OPTIONS: Omit<VideoOptions, 'bandwidth_mbps'> = {
  cameras: [
    { device: '/dev/video0', label: 'HD Capture', signal: '1920×1080　59.94p' },
    { device: '/dev/video2', label: 'USB Camera', signal: '1280×720　29.97p' },
  ],
  files: [
    { path: '/srv/media/colorbars_1080p59.mp4', format: '1920×1080　59.94p', duration_s: 600 },
    { path: '/srv/media/sample_1080p30.mp4', format: '1920×1080　29.97p', duration_s: 185 },
  ],
  bitrate_range_mbps: [1, 40],
  keyframe_range_s: [0.5, 10],
};
const FORMAT: Record<string, string> = {
  '/dev/video0': '1080p59.94',
  '/dev/video2': '720p29.97',
  '/srv/media/colorbars_1080p59.mp4': '1080p59.94',
  '/srv/media/sample_1080p30.mp4': '1080p29.97',
};
const BUFFER_PRESETS = [80, 120, 160, 200, 300, 500, 1000];

function lastRtt() {
  return scenario === 'normal' ? 36 : 38;
}
function guideMs(rtt: number) {
  return rtt * 4;
}
function autoBuffer(rtt: number) {
  return Math.ceil(guideMs(rtt) / 20) * 20;
}
const localBuffer = () => (transport.buffer.mode === 'manual' ? transport.buffer.manual_ms : transport.buffer.auto_ms);

function reset() {
  const t = now();
  liveSince = t - (42 * 60 + 13) * 1000;
  negotiatedAt = liveSince - 1000;
  stoppedAt = null;
  attempt = 5;
  nextRetryAt = t + 4000;
  lastLiveUntil = t - 20000;
  rejectedAt = t - 2000;
  video = {
    version: 1,
    input: { kind: 'camera', camera: '/dev/video0', file: '/srv/media/colorbars_1080p59.mp4', loop: true },
    encode: { mode: 'auto', manual: { ...ENC }, auto: { ...ENC } },
    audio: { enabled: false },
  };
  const rtt = lastRtt();
  transport = {
    version: 1,
    mode: 'outbound',
    peer: { host: '10.20.1.15', port: 9000 },
    listen_port: 9000,
    buffer: { mode: 'auto', manual_ms: autoBuffer(rtt), auto_ms: autoBuffer(rtt), last_rtt_ms: rtt, guide_min_ms: guideMs(rtt) },
    encryption: {
      cipher: 'aes-128',
      passphrase_set: true,
      passphrase_rejected_at: scenario === 'error' ? iso(rejectedAt) : null,
    },
  };
  seedEvents();
  seedHistory();
}

// ------------------------------------------------------------------ status

const isLive = () => stoppedAt === null && scenario !== 'error';

function alerts(): Alert[] {
  if (stoppedAt !== null) return [];
  const t = now();
  if (scenario === 'warn') {
    const list: Alert[] = [{ level: 'warn', code: 'loss_high', since: iso(t - 26000), params: { loss_pct: 2.1, threshold_pct: 1 } }];
    if (role === 'rx') list.push({ level: 'warn', code: 'frames_dropped', since: iso(t - 61000), params: { frames: 3, window_s: 300 } });
    return list;
  }
  if (scenario === 'error') return [{ level: 'error', code: 'peer_rejected_passphrase', since: iso(rejectedAt) }];
  return [];
}

function media(): Media {
  if (role === 'rx') {
    const live = isLive();
    return { kind: 'output', label: 'SDI OUT 1', format: live ? '1080p59.94' : null, preview_fps: 5, audio: 'none', black: !live };
  }
  const label = video.input.kind === 'camera' ? video.input.camera : video.input.file;
  return { kind: video.input.kind, label, format: FORMAT[label] ?? '1080p59.94', preview_fps: 5, audio: 'none', black: false };
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
const jit = (i: number, a: number, p: number) => a * (0.6 * Math.sin(i * 1.7 + p) + 0.4 * Math.sin(i * 0.53 + 2 * p));
const ramp = (i: number, from: number, to: number, start: number) => (i < start ? from : from + ((to - from) * (i - start)) / (N - 1 - start));
const r1 = (v: number) => Math.round(v * 10) / 10;

function sample(i: number): Partial<Record<Metric, number | null>> {
  if (!isLive()) return {};
  const bad = scenario === 'warn';
  const loss = Math.max(0, (bad ? 2.1 : 0.1) + jit(i, bad ? 0.07 : 0.04, 2));
  if (role === 'tx')
    return {
      send_mbps: (bad ? 12.4 : 12.1) + jit(i, 0.06, 1),
      loss_pct: loss,
      rtt_ms: (bad ? 38 : 36) + jit(i, 1.1, 3),
      bandwidth_mbps: (bad ? 41 : 52) + jit(i, 1.4, 4),
    };
  return {
    recv_mbps: (bad ? 11.9 : 12.0) + jit(i, 0.06, 1),
    loss_pct: loss,
    frames_dropped: 0,
    video_latency_ms: (bad ? 312 : 305) + jit(i, 1.5, 5),
    rtt_ms: (bad ? 38 : 36) + jit(i, 1.1, 3),
  };
}

function seedHistory() {
  const bad = scenario !== 'normal';
  const lost = scenario === 'error' ? 56 : N;
  const s = (f: (i: number) => number) => Array.from({ length: N }, (_, i) => (i < lost ? r1(f(i) * 10) / 10 : null));
  const z = () => Array.from({ length: N }, () => null);
  history = {
    send_mbps: role === 'tx' ? s((i) => 12 + jit(i, 0.16, 1) + (bad ? ramp(i, 0, 0.4, 38) : 0)) : z(),
    recv_mbps: role === 'rx' ? s((i) => 12 + jit(i, 0.16, 1) - (bad ? ramp(i, 0, 0.1, 38) : 0)) : z(),
    loss_pct: s((i) => Math.max(0, (bad ? ramp(i, 0.15, 2.1, 38) : 0.1) + jit(i, bad ? 0.07 : 0.04, 2))),
    rtt_ms: s((i) => (bad ? 37.5 : 36) + jit(i, 1.1, 3)),
    bandwidth_mbps: role === 'tx' ? s((i) => (bad ? ramp(i, 52, 41, 36) : 52) + jit(i, 1.4, 4)) : z(),
    video_latency_ms: role === 'rx' ? s((i) => (bad ? 306 + ramp(i, 0, 6, 38) : 305) + jit(i, 1.5, 5)) : z(),
    frames_dropped:
      role === 'rx' ? Array.from({ length: N }, (_, i) => (i < lost ? (bad && (i === 43 || i === 51) ? (i === 43 ? 2 : 1) : 0) : null)) : z(),
  };
}

function pushHistory() {
  const v = sample(tick);
  for (const k of Object.keys(history) as Metric[]) {
    history[k].push(v[k] === undefined || v[k] === null ? null : r1(v[k]! * 10) / 10);
    history[k].shift();
  }
}

function stats(): Stats {
  const live = isLive();
  const v = sample(tick);
  const bad = scenario === 'warn';
  const both = {
    at: iso(now()),
    loss_pct: live ? r1(v.loss_pct!) : null,
    loss_threshold_pct: 1,
    rtt_ms: live ? Math.round(v.rtt_ms!) : null,
    last_rtt_ms: lastRtt(),
    retrans_pct: live ? (bad ? 3.4 : 0.3) : null,
    buffer_ms: role === 'tx' ? localBuffer() : Math.max(localBuffer(), PEER_BUFFER_MS),
    buffer_mode: transport.buffer.mode,
  };
  const target = video.encode.mode === 'manual' ? video.encode.manual.bitrate_mbps : video.encode.auto.bitrate_mbps;
  if (role === 'tx')
    return {
      ...both,
      send_mbps: live ? r1(v.send_mbps!) : null,
      target_mbps: target,
      bandwidth_mbps: live ? Math.round(v.bandwidth_mbps!) : null,
      send_late_5m: live ? (bad ? 18 : 0) : null,
      recv_mbps: null,
      frames_dropped_5m: null,
      video_latency_ms: null,
      unrecovered_5m: null,
    };
  return {
    ...both,
    send_mbps: null,
    target_mbps: null,
    bandwidth_mbps: null,
    send_late_5m: null,
    recv_mbps: live ? r1(v.recv_mbps!) : null,
    frames_dropped_5m: live ? (bad ? 3 : 0) : null,
    video_latency_ms: live ? Math.round(v.video_latency_ms!) : null,
    unrecovered_5m: live ? (bad ? 23 : 0) : null,
  };
}

// ------------------------------------------------------------------ events

function ev(atMs: number, level: Level, code: EventCode, params?: Params): DeviceEvent {
  return { id: `e${++eventSeq}`, at: iso(atMs), level, code, ...(params ? { params } : {}) };
}

function seedEvents() {
  const t = now();
  const ago = (h: number, m: number, s: number) => t - ((h * 60 + m) * 60 + s) * 1000;
  if (role === 'tx') {
    const base = [
      ev(ago(0, 29, 48), 'info', 'bitrate_changed', { mbps: 12 }),
      ev(ago(0, 42, 13), 'info', 'transport_started'),
      ev(ago(0, 42, 14), 'info', 'peer_connected'),
      ev(ago(0, 43, 37), 'info', 'input_detected', { format: '1080p59.94' }),
    ];
    events =
      scenario === 'normal'
        ? base
        : scenario === 'warn'
          ? [ev(ago(0, 0, 26), 'warn', 'loss_high', { loss_pct: 2.1 }), ...base]
          : [
              ev(ago(0, 0, 2), 'error', 'peer_rejected_passphrase'),
              ev(ago(0, 0, 20), 'warn', 'peer_disconnected'),
              ev(ago(0, 0, 26), 'warn', 'loss_high', { loss_pct: 2.1 }),
              ...base.slice(1),
            ];
  } else {
    const base = [
      ev(ago(0, 42, 13), 'info', 'transport_started', { format: '1080p59.94' }),
      ev(ago(0, 42, 14), 'info', 'peer_connected'),
      ev(ago(0, 47, 6), 'info', 'listening_started', { port: 9000 }),
      ev(ago(0, 47, 8), 'info', 'device_started'),
    ];
    events =
      scenario === 'normal'
        ? base
        : scenario === 'warn'
          ? [
              ev(ago(0, 0, 26), 'warn', 'loss_high', { loss_pct: 2.1 }),
              ev(ago(0, 1, 1), 'warn', 'frames_dropped', { frames: 1 }),
              ev(ago(0, 1, 41), 'warn', 'frames_dropped', { frames: 2 }),
              ...base,
            ]
          : [
              ev(ago(0, 0, 2), 'error', 'peer_rejected_passphrase'),
              ev(ago(0, 0, 20), 'warn', 'peer_disconnected'),
              ev(ago(0, 0, 23), 'info', 'passphrase_changed'),
              ...base,
            ];
  }
}

function addEvent(level: Level, code: EventCode, params?: Params) {
  const e = ev(now(), level, code, params);
  events.unshift(e);
  events = events.slice(0, 500);
  broadcast({ type: 'event', event: e });
}

// ------------------------------------------------------------------ settings logic

const effectiveEncode = () => (video.encode.mode === 'auto' ? video.encode.auto : video.encode.manual);

function checkVideo(input: VideoSettingsInput): CheckResult {
  const errors: FieldError[] = [];
  const impacts: Impact[] = [];
  const a = effectiveEncode();
  const m = input.encode.manual;
  const manual = input.encode.mode === 'manual';
  const [bMin, bMax] = OPTIONS.bitrate_range_mbps;
  const [kMin, kMax] = OPTIONS.keyframe_range_s;
  const brValid = typeof m.bitrate_mbps === 'number' && m.bitrate_mbps >= bMin && m.bitrate_mbps <= bMax;
  const kfValid = typeof m.keyframe_s === 'number' && m.keyframe_s >= kMin && m.keyframe_s <= kMax;
  if (manual && !brValid) errors.push({ field: 'encode.manual.bitrate_mbps', code: 'out_of_range', params: { min: bMin, max: bMax } });
  if (manual && !kfValid) errors.push({ field: 'encode.manual.keyframe_s', code: 'out_of_range', params: { min: kMin, max: kMax } });

  const srcC =
    input.input.kind !== video.input.kind ||
    (input.input.kind === 'camera' ? input.input.camera !== video.input.camera : input.input.file !== video.input.file);
  const modeC = input.encode.mode !== video.encode.mode;
  const brC = manual && m.bitrate_mbps !== a.bitrate_mbps;
  const encC =
    manual &&
    (['resolution', 'framerate', 'rate_control', 'codec', 'profile', 'keyframe_s', 'bframes', 'encoder'] as const).some((k) => m[k] !== a[k]);
  const loopC = input.input.kind === 'file' && input.input.loop !== video.input.loop;
  const any = srcC || modeC || brC || encC || loopC;

  if (isLive()) {
    if (srcC) impacts.push({ level: 'warn', code: 'input_switch_gap' });
    if (modeC || brC || encC) impacts.push({ level: 'warn', code: 'encoder_restart_gap' });
    if (any) impacts.push({ level: 'ok', code: 'connection_kept' });
  } else if (any) impacts.push({ level: 'info', code: 'saved_while_stopped' });
  if (modeC && manual) impacts.push({ level: 'info', code: 'manual_fixed' });
  if (modeC && !manual) impacts.push({ level: 'info', code: 'auto_decides' });
  const bw = stats().bandwidth_mbps ?? 52;
  if (brC && brValid && m.bitrate_mbps < a.bitrate_mbps) impacts.push({ level: 'info', code: 'bitrate_lower_quality' });
  if (brC && brValid && m.bitrate_mbps > bw * 0.6) impacts.push({ level: 'warn', code: 'bitrate_near_bandwidth', params: { bandwidth_mbps: bw } });
  return { ok: errors.length === 0, errors, impacts };
}

const IPV4 = /^(25[0-5]|2[0-4]\d|1?\d?\d)(\.(25[0-5]|2[0-4]\d|1?\d?\d)){3}$/;
const portOk = (p: number) => Number.isInteger(p) && p >= 1 && p <= 65535;

function checkTransport(input: TransportSettingsInput): CheckResult {
  const errors: FieldError[] = [];
  const impacts: Impact[] = [];
  if (input.mode !== 'inbound') {
    if (!IPV4.test(input.peer.host)) errors.push({ field: 'peer.host', code: 'invalid' });
    if (!portOk(input.peer.port)) errors.push({ field: 'peer.port', code: 'out_of_range', params: { min: 1, max: 65535 } });
  }
  if (input.mode !== 'outbound' && !portOk(input.listen_port))
    errors.push({ field: 'listen_port', code: 'out_of_range', params: { min: 1, max: 65535 } });
  const pw = input.encryption.passphrase;
  if (pw !== undefined && input.encryption.cipher !== 'none' && (pw.length < 10 || pw.length > 79))
    errors.push({ field: 'encryption.passphrase', code: pw.length < 10 ? 'too_short' : 'too_long', params: { min: 10, max: 79 } });
  if (input.buffer.mode === 'manual' && !BUFFER_PRESETS.includes(input.buffer.manual_ms))
    errors.push({ field: 'buffer.manual_ms', code: 'invalid' });

  const st = status().state;
  if (st === 'live') impacts.push({ level: 'warn', code: 'reconnect_gap' });
  else if (st === 'stopped') impacts.push({ level: 'info', code: 'saved_while_stopped' });
  else {
    impacts.push({ level: 'info', code: 'reconnect' });
    impacts.push({ level: 'ok', code: 'no_peer_effect' });
  }
  const guide = transport.buffer.guide_min_ms;
  if (input.buffer.mode === 'manual' && guide !== null && input.buffer.manual_ms < guide)
    impacts.push({ level: 'warn', code: 'buffer_below_guide', params: { guide_ms: guide } });
  return { ok: errors.length === 0, errors, impacts };
}

function compare(): CompareResult {
  const local = localBuffer();
  const known = negotiatedAt > 0;
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
      { key: 'buffer', local_ms: local, local_mode: transport.buffer.mode, peer_ms: PEER_BUFFER_MS, effective_ms: Math.max(local, PEER_BUFFER_MS), verdict: 'effective' },
      { key: 'cipher', local: transport.encryption.cipher, peer: PEER_CIPHER, verdict: transport.encryption.cipher === PEER_CIPHER ? 'match' : 'mismatch' },
      {
        key: 'passphrase',
        local_set: transport.encryption.passphrase_set,
        verdict: scenario === 'error' ? 'mismatch' : stoppedAt !== null ? 'unknown' : 'match',
        reason: scenario === 'error' ? 'peer_rejected' : null,
      },
    ],
  };
}

// ------------------------------------------------------------------ network test

const tests = new Map<string, NetworkTest & { startMs: number }>();

function networkTest(id: string): NetworkTest | null {
  const t = tests.get(id);
  if (!t) return null;
  const dur = 5000;
  const p = Math.min(1, (now() - t.startMs) / dur);
  if (scenario === 'error' && p > 0.2) return { ...strip(t), state: 'failed', progress: p, error: 'peer_unreachable' };
  if (p < 1) return { ...strip(t), progress: p };
  if (t.state === 'running') {
    t.state = 'done';
    t.result = scenario === 'warn' ? { throughput_mbps: 39.6, loss_pct: 1.9, jitter_ms: 3.1 } : { throughput_mbps: 48.2, loss_pct: 0.2, jitter_ms: 1.3 };
    addEvent('info', 'network_test_done', { mbps: t.result.throughput_mbps });
  }
  return { ...strip(t), progress: 1 };
}
const strip = (t: NetworkTest & { startMs: number }): NetworkTest => {
  const { startMs: _ignored, ...rest } = t;
  void _ignored;
  return rest;
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

function mjpeg(req: IncomingMessage, res: ServerResponse) {
  res.writeHead(200, {
    'Content-Type': 'multipart/x-mixed-replace; boundary=frame',
    'Cache-Control': 'no-store',
    Connection: 'close',
  });
  let i = 0;
  const write = () => {
    const buf = frames[i++ % frames.length];
    res.write(`--frame\r\nContent-Type: image/jpeg\r\nContent-Length: ${buf.length}\r\n\r\n`);
    res.write(buf);
    res.write('\r\n');
  };
  write();
  const timer = setInterval(write, 200); // ≈5 fps
  req.on('close', () => clearInterval(timer));
}

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
    if (m === 'GET' && path === '/preview.mjpeg') return mjpeg(req, res);

    if (m === 'POST' && path === '/transport/stop') {
      if (stoppedAt === null) {
        stoppedAt = now();
        addEvent('info', 'transport_stopped');
      }
      broadcast({ type: 'status', status: status() });
      return send(res, 200, status());
    }
    if (m === 'POST' && path === '/transport/start') {
      if (stoppedAt !== null) {
        stoppedAt = null;
        liveSince = now();
        nextRetryAt = now() + 5000;
        addEvent('info', 'transport_started', role === 'rx' ? { format: '1080p59.94' } : undefined);
      }
      broadcast({ type: 'status', status: status() });
      return send(res, 200, status());
    }
    if (m === 'POST' && path === '/transport/retry') {
      if (scenario === 'error' && stoppedAt === null) {
        attempt += 1;
        nextRetryAt = now() + 5000;
        rejectedAt = now();
        transport.encryption.passphrase_rejected_at = iso(rejectedAt);
        addEvent('error', 'peer_rejected_passphrase');
      }
      broadcast({ type: 'status', status: status() });
      return send(res, 200, status());
    }

    // video (transmitter)
    if (path.startsWith('/settings/video') && role !== 'tx') return send(res, 409, { error: { code: 'unsupported_role' } });
    if (m === 'GET' && path === '/settings/video') {
      video.encode.auto.bitrate_mbps = 12;
      return send(res, 200, video);
    }
    if (m === 'GET' && path === '/settings/video/options') return send(res, 200, { ...OPTIONS, bandwidth_mbps: stats().bandwidth_mbps });
    if (m === 'POST' && path === '/settings/video/check') return send(res, 200, checkVideo(await readJson<VideoSettingsInput>(req)));
    if (m === 'PUT' && path === '/settings/video') {
      const input = await readJson<VideoSettingsInput>(req);
      if (input.version !== video.version) return send(res, 409, { error: { code: 'version_conflict' } });
      const c = checkVideo(input);
      if (!c.ok) return send(res, 400, { error: { code: 'invalid', errors: c.errors } });
      const prevBr = effectiveEncode().bitrate_mbps;
      await sleep(1200); // encoder re-initialisation
      video = {
        ...video,
        version: video.version + 1,
        input: { ...input.input },
        encode: { mode: input.encode.mode, manual: input.encode.mode === 'manual' ? { ...input.encode.manual } : video.encode.manual, auto: video.encode.auto },
      };
      addEvent('info', 'video_settings_applied');
      if (effectiveEncode().bitrate_mbps !== prevBr) addEvent('info', 'bitrate_changed', { mbps: effectiveEncode().bitrate_mbps });
      broadcast({ type: 'settings', section: 'video', version: video.version });
      broadcast({ type: 'status', status: status() });
      const out: ApplyResult<VideoSettings> = { settings: video, applied_at: iso(now()), impacts: c.impacts };
      return send(res, 200, out);
    }

    // transport (transmitter)
    if ((path.startsWith('/settings/transport') || path === '/transport/compare') && role !== 'tx')
      return send(res, 409, { error: { code: 'unsupported_role' } });
    if (m === 'GET' && path === '/settings/transport') return send(res, 200, transport);
    if (m === 'GET' && path === '/settings/transport/options') return send(res, 200, { buffer_presets_ms: BUFFER_PRESETS });
    if (m === 'GET' && path === '/transport/compare') return send(res, 200, compare());
    if (m === 'POST' && path === '/settings/transport/check') return send(res, 200, checkTransport(await readJson<TransportSettingsInput>(req)));
    if (m === 'PUT' && path === '/settings/transport') {
      const input = await readJson<TransportSettingsInput>(req);
      if (input.version !== transport.version) return send(res, 409, { error: { code: 'version_conflict' } });
      const c = checkTransport(input);
      if (!c.ok) return send(res, 400, { error: { code: 'invalid', errors: c.errors } });
      await sleep(1000);
      const newPass = input.encryption.passphrase !== undefined && input.encryption.cipher !== 'none';
      transport = {
        ...transport,
        version: transport.version + 1,
        mode: input.mode,
        peer: { ...input.peer },
        listen_port: input.listen_port,
        buffer: { ...transport.buffer, mode: input.buffer.mode, manual_ms: input.buffer.manual_ms },
        encryption: {
          cipher: input.encryption.cipher,
          passphrase_set: input.encryption.cipher === 'none' ? transport.encryption.passphrase_set : transport.encryption.passphrase_set || newPass,
          passphrase_rejected_at: newPass ? null : transport.encryption.passphrase_rejected_at,
        },
      };
      addEvent('info', 'transport_settings_applied');
      if (newPass) addEvent('info', 'passphrase_changed');
      broadcast({ type: 'settings', section: 'transport', version: transport.version });
      // A new passphrase in the error scenario lets the link come back after a moment.
      if (newPass && scenario === 'error' && stoppedAt === null) {
        setTimeout(() => {
          scenario = 'normal';
          liveSince = now();
          negotiatedAt = now();
          addEvent('info', 'peer_connected');
          addEvent('info', 'transport_started');
          broadcast({ type: 'status', status: status() });
        }, 2000);
      }
      const out: ApplyResult<TransportSettings> = { settings: transport, applied_at: iso(now()), impacts: c.impacts };
      return send(res, 200, out);
    }

    // network test
    if (m === 'POST' && path === '/network-tests') {
      if (role !== 'tx') return send(res, 409, { error: { code: 'unsupported_role' } });
      const b = await readJson<{ direction?: 'to_peer' | 'from_peer' }>(req);
      const id = `t${Date.now()}`;
      const t = { id, state: 'running' as const, direction: b.direction ?? 'to_peer', started_at: iso(now()), progress: 0, result: null, error: null, startMs: now() };
      tests.set(id, t);
      return send(res, 202, strip(t));
    }
    const nt = path.match(/^\/network-tests\/([\w-]+)$/);
    if (m === 'GET' && nt) {
      const t = networkTest(nt[1]);
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
  if (scenario === 'error' && stoppedAt === null && role === 'tx' && now() >= nextRetryAt) {
    attempt += 1;
    nextRetryAt = now() + 5000;
  }
  if (tick % 5 === 0) pushHistory();
  broadcast({ type: 'stats', stats: stats() });
  broadcast({ type: 'status', status: status() });
}, 1000);

reset();
server.listen(PORT, () => {
  console.log(`mock device API: http://127.0.0.1:${PORT}/api/v1  role=${role} scenario=${scenario}`);
});
