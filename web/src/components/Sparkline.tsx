/**
 * 5-minute trend next to each readout. Gaps (`null`) are left blank, never interpolated.
 * The vertical scale follows the data, but never spans less than `minSpan`, so small
 * jitter is not blown up into a dramatic line.
 */
export function Sparkline({
  values,
  reference,
  threshold,
  minSpan,
  floor,
  ticks,
  label,
}: {
  values: (number | null)[];
  /** dashed series drawn behind the line (e.g. the target the measured value follows) */
  reference?: (number | null)[];
  /** dashed horizontal line (e.g. the level below which a warning is raised) */
  threshold?: number;
  /** smallest range of the vertical axis, in the unit of the values */
  minSpan: number;
  /** the axis never goes below this (e.g. 0 for rates) */
  floor?: number;
  /** draw short vertical marks where values > 0 (e.g. picture stops) instead of a line */
  ticks?: boolean;
  label: string;
}) {
  const W = 112;
  const n = Math.max(values.length, 2);
  const x = (i: number) => ((i / (n - 1)) * W).toFixed(1);

  // Scale: data (and reference, threshold) range, widened to minSpan around its middle.
  const seen = [...values, ...(reference ?? []), ...(threshold !== undefined ? [threshold] : [])].filter(
    (v): v is number => v !== null && Number.isFinite(v),
  );
  let lo = seen.length ? Math.min(...seen) : 0;
  let hi = seen.length ? Math.max(...seen) : minSpan;
  if (hi - lo < minSpan) {
    const mid = (hi + lo) / 2;
    lo = mid - minSpan / 2;
    hi = mid + minSpan / 2;
  }
  const pad = (hi - lo) * 0.08;
  lo -= pad;
  hi += pad;
  if (floor !== undefined && lo < floor) {
    hi += floor - lo;
    lo = floor;
  }
  const y = (v: number) => (28 - ((Math.min(Math.max(v, lo), hi) - lo) / (hi - lo)) * 26).toFixed(1);
  const path = (vs: (number | null)[]) => {
    let d = '';
    vs.forEach((v, i) => {
      if (v === null) return;
      d += (i > 0 && vs[i - 1] !== null ? 'L' : 'M') + x(i) + ' ' + y(v);
    });
    return d;
  };

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
    line = path(values);
  }

  return (
    <svg className="spark" width={W} height={30} viewBox={`0 0 ${W} 30`} role="img" aria-label={label}>
      <title>{label}</title>
      {threshold !== undefined && !ticks && (
        <line x1={0} y1={y(threshold)} x2={W} y2={y(threshold)} stroke="var(--tick)" strokeWidth={1} strokeDasharray="3 3" />
      )}
      {reference && !ticks && <path d={path(reference)} fill="none" stroke="var(--tick)" strokeWidth={1} strokeDasharray="3 3" />}
      <path d={line} fill="none" stroke="var(--sub)" strokeWidth={1.5} strokeLinejoin="round" strokeLinecap="round" />
      {marks && <path d={marks} fill="none" stroke="var(--warn-text)" strokeWidth={2} strokeLinecap="round" />}
    </svg>
  );
}
