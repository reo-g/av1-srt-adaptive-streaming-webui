import { useEffect, useState, type ReactNode } from 'react';
import { useDrafts, useLive } from '../api/live';
import type { Role, Status } from '../api/types';
import { alertLabel, alertText, roleName, stateLabel } from '../i18n/ja';
import { duration, timeOf } from '../lib/format';
import { href, type Route } from '../lib/router';
import { Glyph, toneText, type Tone } from './Glyph';

/** Device clock, ticking once a second (the device clock, not the browser's). */
export function useDeviceNow(): number {
  const { clockOffsetMs } = useLive();
  const [now, setNow] = useState(() => Date.now() + clockOffsetMs);
  useEffect(() => {
    setNow(Date.now() + clockOffsetMs);
    const t = setInterval(() => setNow(Date.now() + clockOffsetMs), 1000);
    return () => clearInterval(t);
  }, [clockOffsetMs]);
  return now;
}

export function stateBadge(role: Role, s: Status, now: number): { tone: Tone; label: string; sub: string } {
  const label = stateLabel(role, s.state);
  switch (s.state) {
    case 'live':
      return { tone: 'ok', label, sub: duration((now - Date.parse(s.since)) / 1000) };
    case 'reconnecting':
      return { tone: 'warn', label, sub: s.attempt ? `${s.attempt} 回目` : '' };
    case 'connecting':
      return { tone: 'off', label, sub: '' };
    case 'listening':
      return { tone: 'off', label, sub: s.listen_port ? `ポート ${s.listen_port}` : '' };
    case 'stopped':
      return { tone: 'off', label, sub: `${timeOf(s.since)} から` };
  }
}

function Header() {
  const { device, status } = useLive();
  const now = useDeviceNow();
  const badge = device && status ? stateBadge(device.role, status, now) : null;
  return (
    <header className="hdr">
      <div className="hdr-id">
        <b>{device?.id ?? '—'}</b>
        <span className="muted">{device ? roleName(device.role) : ''}</span>
        <span style={{ color: 'var(--muted)' }}>{device?.address ?? ''}</span>
      </div>
      <div className="hdr-state">
        {badge && (
          <>
            <Glyph tone={badge.tone} />
            <span style={{ fontWeight: 600, color: toneText[badge.tone] }}>{badge.label}</span>
            <span className="muted">{badge.sub}</span>
            <span aria-hidden="true" className="hdr-sep" />
          </>
        )}
        <span className="clock" aria-label={`機器の時刻 ${timeOf(now)} 日本時間`}>
          {timeOf(now)}
          <small>JST</small>
        </span>
      </div>
    </header>
  );
}

function NavLink({ route, current, children, count }: { route: Route; current: Route; children: ReactNode; count?: number }) {
  return (
    <a href={href(route)} aria-current={route === current ? 'page' : undefined}>
      {children}
      {count ? (
        <span className="count-badge" aria-label={`未適用の変更 ${count} 件`}>
          {count}
        </span>
      ) : null}
    </a>
  );
}

function Nav({ current }: { current: Route }) {
  const { counts } = useDrafts();
  return (
    <nav className="nav" aria-label="メニュー">
      <NavLink route="ops" current={current}>
        運用
      </NavLink>
      <div className="nav-group">設定</div>
      <NavLink route="video" current={current} count={counts.video}>
        映像
      </NavLink>
      <NavLink route="transport" current={current} count={counts.transport}>
        伝送
      </NavLink>
      <NavLink route="recording" current={current}>
        録画
      </NavLink>
      <div className="nav-group">管理</div>
      <NavLink route="network" current={current}>
        ネットワーク
      </NavLink>
      <NavLink route="logs" current={current}>
        ログ
      </NavLink>
      <NavLink route="device" current={current}>
        機器情報
      </NavLink>
    </nav>
  );
}

/** The thin status strip under the header. Warnings and errors never auto-hide. */
export function AlertStrip({ link }: { link?: boolean }) {
  const { device, status } = useLive();
  if (!device || !status || status.alerts.length === 0) return null;
  const worst = status.alerts.some((a) => a.level === 'error') ? 'error' : 'warn';
  const lead = status.alerts.find((a) => a.level === worst)!;
  const text = status.alerts
    .filter((a) => a.level === worst || worst === 'warn')
    .map((a) => alertText(device.role, a, status.peer.id))
    .join('');
  const tone: Tone = worst === 'error' ? 'err' : 'warn';
  return (
    <section role="status" aria-label="現在の状況" className={`strip tone-${tone}`}>
      <p>
        <span className="strip-label">
          <Glyph tone={tone} size={12} />
          {alertLabel(lead)}
        </span>
        <span className="strip-text">{text}</span>
      </p>
      {link && <a href={href('transport')}>伝送設定を開く</a>}
    </section>
  );
}

/**
 * Page frame: header, left menu, an optional banner under the header
 * (by default the alert strip), then the page content.
 */
export function Shell({ route, banner, children }: { route: Route; banner?: ReactNode; children: ReactNode }) {
  return (
    <div className="app">
      <Header />
      <div className="layout">
        <Nav current={route} />
        <div className="content">
          {banner === undefined ? <AlertStrip link={route !== 'transport'} /> : banner}
          <main className="main">{children}</main>
        </div>
      </div>
    </div>
  );
}
