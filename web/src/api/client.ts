import type {
  ApiError,
  ApplyResult,
  CheckResult,
  CompareResult,
  Device,
  DeviceEvent,
  NetworkTest,
  NetworkTestList,
  NetworkTestOptions,
  NetworkTestRequest,
  PreviewOffer,
  PreviewSession,
  Stats,
  StatsHistory,
  Status,
  TransportOptions,
  TransportSettings,
  TransportSettingsInput,
  VideoOptions,
  VideoSettings,
  VideoSettingsInput,
} from './types';

const BASE = '/api/v1';

export class ApiFailure extends Error {
  constructor(
    readonly status: number,
    readonly body: ApiError | null,
  ) {
    super(body?.error.code ?? `HTTP ${status}`);
  }
}

async function req<T>(
  method: 'GET' | 'POST' | 'PUT' | 'DELETE',
  path: string,
  body?: unknown,
  init?: Pick<RequestInit, 'keepalive'>,
): Promise<T> {
  const res = await fetch(BASE + path, {
    method,
    headers: body === undefined ? undefined : { 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
    ...init,
  });
  if (!res.ok) {
    let parsed: ApiError | null = null;
    try {
      parsed = (await res.json()) as ApiError;
    } catch {
      parsed = null;
    }
    throw new ApiFailure(res.status, parsed);
  }
  return (res.status === 204 ? undefined : await res.json()) as T;
}

export const api = {
  device: () => req<Device>('GET', '/device'),
  status: () => req<Status>('GET', '/status'),
  stats: () => req<Stats>('GET', '/stats'),
  history: (windowS = 300) => req<StatsHistory>('GET', `/stats/history?window_s=${windowS}`),
  events: (limit = 20) => req<DeviceEvent[]>('GET', `/events?limit=${limit}`),

  start: () => req<Status>('POST', '/transport/start'),
  stop: () => req<Status>('POST', '/transport/stop'),
  retry: () => req<Status>('POST', '/transport/retry'),

  video: {
    get: () => req<VideoSettings>('GET', '/settings/video'),
    options: () => req<VideoOptions>('GET', '/settings/video/options'),
    check: (input: VideoSettingsInput) => req<CheckResult>('POST', '/settings/video/check', input),
    apply: (input: VideoSettingsInput) => req<ApplyResult<VideoSettings>>('PUT', '/settings/video', input),
  },
  transport: {
    get: () => req<TransportSettings>('GET', '/settings/transport'),
    options: () => req<TransportOptions>('GET', '/settings/transport/options'),
    check: (input: TransportSettingsInput) => req<CheckResult>('POST', '/settings/transport/check', input),
    apply: (input: TransportSettingsInput) =>
      req<ApplyResult<TransportSettings>>('PUT', '/settings/transport', input),
    compare: () => req<CompareResult>('GET', '/transport/compare'),
  },
  networkTest: {
    start: (body: NetworkTestRequest) => req<NetworkTest>('POST', '/network-tests', body),
    list: () => req<NetworkTestList>('GET', '/network-tests'),
    get: (id: string) => req<NetworkTest>('GET', `/network-tests/${encodeURIComponent(id)}`),
    options: () => req<NetworkTestOptions>('GET', '/network-tests/options'),
    /** Make a finished line-speed test (towards the peer) the send cap; answers the new transport settings. */
    use: (id: string) => req<TransportSettings>('POST', `/network-tests/${encodeURIComponent(id)}/use`),
  },

  /** Open a preview session: send the browser's offer, get the device's answer. */
  startPreview: (offer: string) => req<PreviewSession>('POST', '/preview/sessions', { offer } satisfies PreviewOffer),
  /** Close it. keepalive lets the request finish when the page is being closed. */
  stopPreview: (id: string) => req<void>('DELETE', `/preview/sessions/${encodeURIComponent(id)}`, undefined, { keepalive: true }),
};

export const liveUrl = () => {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  return `${proto}//${location.host}${BASE}/live`;
};
