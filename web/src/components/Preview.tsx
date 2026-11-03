import { useEffect, useState } from 'react';
import { previewUrl } from '../api/client';
import type { Media, Role } from '../api/types';

/** Stop pulling preview frames while the tab is in the background (device load). */
export function usePageVisible(): boolean {
  const [visible, setVisible] = useState(() => document.visibilityState === 'visible');
  useEffect(() => {
    const on = () => setVisible(document.visibilityState === 'visible');
    document.addEventListener('visibilitychange', on);
    return () => document.removeEventListener('visibilitychange', on);
  }, []);
  return visible;
}

export const mediaMeta = (m: Media): string => {
  if (m.kind === 'output') {
    if (m.black || !m.format) return `${m.label}　黒を出力中`;
    return `${m.label}　${m.format}　約 ${m.preview_fps} fps で表示`;
  }
  const kind = m.kind === 'camera' ? 'カメラ' : '映像ファイル';
  return m.format ? `${kind} ${m.label}　${m.format}　約 ${m.preview_fps} fps で表示` : `${kind} ${m.label}　信号なし`;
};

/** Whether there is a picture to show: tx shows its input, rx shows what it outputs. */
export const hasPicture = (m: Media) => !!m.format && !m.black;

export function PreviewImage({ media, role, className }: { media: Media; role: Role; className?: string }) {
  const visible = usePageVisible();
  const [failed, setFailed] = useState(false);
  const show = hasPicture(media) && visible && !failed;
  useEffect(() => setFailed(false), [media.format, media.black]);
  const placeholder = role === 'tx' ? '入力映像' : hasPicture(media) ? '受信映像' : '受信映像なし';
  return (
    <div className={`${className ?? 'preview'}${hasPicture(media) ? '' : ' empty'}`}>
      <span>{placeholder}</span>
      {show && <img src={previewUrl()} alt={role === 'tx' ? '入力映像のプレビュー' : '受信映像のプレビュー'} onError={() => setFailed(true)} />}
    </div>
  );
}
