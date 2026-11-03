/**
 * Status shapes. A state is never shown by colour alone: the shape differs too,
 * and a text label always sits next to it.
 *   ● ok   ▲ warn   ■ err   ○ off   — na (not measured)   • info
 */
export type Tone = 'ok' | 'warn' | 'err' | 'off' | 'na' | 'info';

const SHAPE: Record<Tone, { d: string; fill: string; stroke: string }> = {
  ok: { d: 'M1.5 6a4.5 4.5 0 1 0 9 0a4.5 4.5 0 1 0 -9 0Z', fill: 'var(--ok)', stroke: 'none' },
  warn: { d: 'M6 1.2L11 10.4H1Z', fill: 'var(--warn)', stroke: 'none' },
  err: { d: 'M2 2H10V10H2Z', fill: 'var(--err)', stroke: 'none' },
  off: { d: 'M2 6a4 4 0 1 0 8 0a4 4 0 1 0 -8 0Z', fill: 'none', stroke: 'var(--sub)' },
  na: { d: 'M2.5 6H9.5', fill: 'none', stroke: 'var(--sub)' },
  info: { d: 'M3.5 6a2.5 2.5 0 1 0 5 0a2.5 2.5 0 1 0 -5 0Z', fill: 'var(--muted)', stroke: 'none' },
};

/** Text colour that goes with each tone. */
export const toneText: Record<Tone, string> = {
  ok: 'var(--ok)',
  warn: 'var(--warn-text)',
  err: 'var(--err-text)',
  off: 'var(--sub)',
  na: 'var(--sub)',
  info: 'var(--sub)',
};

export function Glyph({ tone, size = 10, dark }: { tone: Tone; size?: number; dark?: boolean }) {
  const s = SHAPE[tone];
  // Brighter variants for the dark full-screen band.
  const fill = dark ? ({ ok: '#4cc38a', warn: '#f0a93b', err: '#f2685c' } as Record<string, string>)[tone] ?? s.fill : s.fill;
  const stroke = dark && s.stroke !== 'none' ? '#aeb6c2' : s.stroke;
  return (
    <svg width={size} height={size} viewBox="0 0 12 12" aria-hidden="true" style={{ flex: '0 0 auto' }}>
      <path d={s.d} fill={fill} stroke={stroke} strokeWidth={1.5} />
    </svg>
  );
}

export const Chevron = () => (
  <svg width="10" height="10" viewBox="0 0 12 12" aria-hidden="true">
    <path d="M4 2l4 4-4 4" fill="none" stroke="currentColor" strokeWidth="1.6" />
  </svg>
);

export const FullscreenIcon = ({ exit }: { exit?: boolean }) => (
  <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true">
    <path
      d={exit ? 'M5 1.5V5H1.5M12.5 5H9V1.5M9 12.5V9h3.5M1.5 9H5v3.5' : 'M1.5 5V1.5H5M9 1.5h3.5V5M12.5 9v3.5H9M5 12.5H1.5V9'}
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
    />
  </svg>
);
