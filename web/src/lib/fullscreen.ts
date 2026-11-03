import { go } from './router';

/**
 * Full-screen view (docs/screens.md「全画面表示」).
 * Uses the browser Fullscreen API when allowed; when it is refused (some tablets,
 * embedded views) the same view is shown maximised inside the page instead.
 */
export function openFullscreen() {
  sessionStorage.setItem('fs-return-focus', '1');
  const el = document.documentElement;
  try {
    const p = el.requestFullscreen?.({ navigationUI: 'hide' });
    p?.catch(() => undefined);
  } catch {
    /* in-page fallback */
  }
  go('fullscreen');
}

export function closeFullscreen() {
  if (document.fullscreenElement) document.exitFullscreen().catch(() => undefined);
  go('ops');
}

/** After returning to the operation screen, focus goes back to the 全画面 button. */
export function takeReturnFocus(): boolean {
  const v = sessionStorage.getItem('fs-return-focus') === '1';
  sessionStorage.removeItem('fs-return-focus');
  return v;
}
