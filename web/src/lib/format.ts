/** Times are always shown in the device zone (JST), whatever the browser's zone is. */
const ZONE = 'Asia/Tokyo';

const hms = new Intl.DateTimeFormat('ja-JP', {
  timeZone: ZONE,
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
  hourCycle: 'h23',
});

export const timeOf = (iso: string | number | Date): string => hms.format(new Date(iso));

const mdhm = new Intl.DateTimeFormat('ja-JP', {
  timeZone: ZONE,
  month: '2-digit',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit',
  hourCycle: 'h23',
});

/** "11/04 14:50": for a moment that may be days ago (add "JST" where it is shown). */
export const dateTimeOf = (iso: string | number | Date): string => {
  const p = Object.fromEntries(mdhm.formatToParts(new Date(iso)).map((x) => [x.type, x.value]));
  return `${p.month}/${p.day} ${p.hour}:${p.minute}`;
};

export const duration = (seconds: number): string => {
  const s = Math.max(0, Math.floor(seconds));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const r = s % 60;
  return [h, m, r].map((x) => String(x).padStart(2, '0')).join(':');
};

export const fixed1 = (v: number) => v.toFixed(1);
export const fixed2 = (v: number) => v.toFixed(2);
export const int = (v: number) => String(Math.round(v));

/** The file name without its folder: the UI never shows device paths. */
export const fileName = (path: string) => path.slice(path.lastIndexOf('/') + 1);
