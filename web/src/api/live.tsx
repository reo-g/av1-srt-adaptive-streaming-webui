import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { api, liveUrl } from './client';
import type { Device, DeviceEvent, LiveMessage, Stats, StatsHistory, Status } from './types';

interface LiveState {
  device: Device | null;
  status: Status | null;
  stats: Stats | null;
  history: StatsHistory | null;
  events: DeviceEvent[];
  /** device clock minus browser clock, in ms */
  clockOffsetMs: number;
  connected: boolean;
  /** Re-read everything over REST (after a command such as stop/start). */
  refresh: () => Promise<void>;
  setStatus: (s: Status) => void;
}

const LiveContext = createContext<LiveState | null>(null);

export function LiveProvider({ children }: { children: ReactNode }) {
  const [device, setDevice] = useState<Device | null>(null);
  const [status, setStatusState] = useState<Status | null>(null);
  const [stats, setStats] = useState<Stats | null>(null);
  const [history, setHistory] = useState<StatsHistory | null>(null);
  const [events, setEvents] = useState<DeviceEvent[]>([]);
  const [clockOffsetMs, setOffset] = useState(0);
  const [connected, setConnected] = useState(false);

  const setStatus = useCallback((s: Status) => {
    setStatusState(s);
    setOffset(Date.parse(s.server_time) - Date.now());
  }, []);

  const refresh = useCallback(async () => {
    const [d, s, st, h, ev] = await Promise.all([
      api.device(),
      api.status(),
      api.stats(),
      api.history(300),
      api.events(20),
    ]);
    setDevice(d);
    setStatus(s);
    setStats(st);
    setHistory(h);
    setEvents(ev);
  }, [setStatus]);

  // Initial load + history refresh every 5 s (the history interval).
  useEffect(() => {
    refresh().catch(() => undefined);
    const t = setInterval(() => {
      api.history(300).then(setHistory).catch(() => undefined);
    }, 5000);
    return () => clearInterval(t);
  }, [refresh]);

  // Live channel with reconnect.
  const retry = useRef(0);
  useEffect(() => {
    let ws: WebSocket | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let closed = false;
    const open = () => {
      ws = new WebSocket(liveUrl());
      ws.onopen = () => {
        retry.current = 0;
        setConnected(true);
      };
      ws.onmessage = (m) => {
        const msg = JSON.parse(String(m.data)) as LiveMessage;
        switch (msg.type) {
          case 'hello':
            setDevice(msg.device);
            setStatus(msg.status);
            setStats(msg.stats);
            break;
          case 'status':
            setStatus(msg.status);
            break;
          case 'stats':
            setStats(msg.stats);
            break;
          case 'event':
            setEvents((prev) => [msg.event, ...prev.filter((e) => e.id !== msg.event.id)].slice(0, 50));
            break;
          case 'settings':
            break;
        }
      };
      ws.onclose = () => {
        setConnected(false);
        if (closed) return;
        const wait = Math.min(10000, 500 * 2 ** retry.current++);
        timer = setTimeout(open, wait);
      };
    };
    open();
    return () => {
      closed = true;
      if (timer) clearTimeout(timer);
      ws?.close();
    };
  }, [setStatus]);

  const value = useMemo(
    () => ({ device, status, stats, history, events, clockOffsetMs, connected, refresh, setStatus }),
    [device, status, stats, history, events, clockOffsetMs, connected, refresh, setStatus],
  );
  return <LiveContext.Provider value={value}>{children}</LiveContext.Provider>;
}

export function useLive(): LiveState {
  const v = useContext(LiveContext);
  if (!v) throw new Error('useLive outside LiveProvider');
  return v;
}

// ---------------------------------------------------------------- drafts

/**
 * Unapplied edits survive navigation between screens, so the menu can show
 * "未適用の変更 n 件" badges and nothing typed is lost by opening another page.
 */
interface Drafts {
  counts: Record<'video' | 'transport', number>;
  setCount: (key: 'video' | 'transport', n: number) => void;
  store: Record<string, unknown>;
  put: (key: string, v: unknown) => void;
}

const DraftContext = createContext<Drafts | null>(null);

export function DraftProvider({ children }: { children: ReactNode }) {
  const [counts, setCounts] = useState<Drafts['counts']>({ video: 0, transport: 0 });
  const [store, setStore] = useState<Record<string, unknown>>({});
  const setCount = useCallback((key: 'video' | 'transport', n: number) => {
    setCounts((c) => (c[key] === n ? c : { ...c, [key]: n }));
  }, []);
  const put = useCallback((key: string, v: unknown) => setStore((s) => ({ ...s, [key]: v })), []);

  // Warn before closing the tab with unapplied edits.
  useEffect(() => {
    const any = counts.video + counts.transport > 0;
    if (!any) return;
    const h = (e: BeforeUnloadEvent) => {
      e.preventDefault();
    };
    addEventListener('beforeunload', h);
    return () => removeEventListener('beforeunload', h);
  }, [counts]);

  const value = useMemo(() => ({ counts, setCount, store, put }), [counts, setCount, store, put]);
  return <DraftContext.Provider value={value}>{children}</DraftContext.Provider>;
}

export function useDrafts(): Drafts {
  const v = useContext(DraftContext);
  if (!v) throw new Error('useDrafts outside DraftProvider');
  return v;
}

/** useState that lives in the draft store (kept while the user visits other screens). */
export function useDraftState<T>(key: string, initial: T | null): [T | null, (v: T | null) => void] {
  const { store, put } = useDrafts();
  const value = (key in store ? (store[key] as T | null) : initial) ?? null;
  const set = useCallback((v: T | null) => put(key, v), [key, put]);
  return [value, set];
}
