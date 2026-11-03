/** 5-minute trend next to each readout. Gaps (`null`) are left blank, never interpolated. */
export function Sparkline({
  values,
  lo,
  hi,
  threshold,
  ticks,
  label,
}: {
  values: (number | null)[];
  lo: number;
  hi: number;
  /** dashed reference line (the threshold) */
  threshold?: number;
  /** draw short vertical marks where values > 0 (e.g. dropped frames) instead of a line */
  ticks?: boolean;
  label: string;
}) {
  const W = 112;
  const n = Math.max(values.length, 2);
  const x = (i: number) => ((i / (n - 1)) * W).toFixed(1);
  const y = (v: number) => (28 - ((Math.min(Math.max(v, lo), hi) - lo) / (hi - lo)) * 26).toFixed(1);

  let line = '';
  let marks = '';
  if (ticks) {
    // baseline up to the last measured point, plus a tick for each event
    let last = -1;
    values.forEach((v, i) => {
      if (v !== null) last = i;
      if (v !== null && v > 0) marks += `M${x(i)} 26V10`;
    });
    if (last >= 0) line = `M0 28H${x(last)}`;
  } else {
    values.forEach((v, i) => {
      if (v === null) return;
      line += (i > 0 && values[i - 1] !== null ? 'L' : 'M') + x(i) + ' ' + y(v);
    });
  }

  return (
    <svg className="spark" width={W} height={30} viewBox={`0 0 ${W} 30`} role="img" aria-label={label}>
      <title>{label}</title>
      {threshold !== undefined && (
        <line x1={0} y1={y(threshold)} x2={W} y2={y(threshold)} stroke="var(--tick)" strokeWidth={1} strokeDasharray="3 3" />
      )}
      <path d={line} fill="none" stroke="var(--sub)" strokeWidth={1.5} strokeLinejoin="round" strokeLinecap="round" />
      {marks && <path d={marks} fill="none" stroke="var(--warn-text)" strokeWidth={2} strokeLinecap="round" />}
    </svg>
  );
}
