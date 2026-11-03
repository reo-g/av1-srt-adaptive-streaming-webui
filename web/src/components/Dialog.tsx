import { useEffect, useId, useRef, type ReactNode } from 'react';

/**
 * Alert dialog for destructive or important actions.
 * Focus moves into the dialog (to the safe choice), Tab stays inside, Esc cancels,
 * and focus returns to the element that opened it.
 */
export function AlertDialog({
  title,
  description,
  children,
  actions,
  onCancel,
  wide,
  busy,
}: {
  title: ReactNode;
  description?: ReactNode;
  children?: ReactNode;
  actions: ReactNode;
  onCancel: () => void;
  wide?: boolean;
  busy?: boolean;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const titleId = useId();
  const descId = useId();

  useEffect(() => {
    const opener = document.activeElement as HTMLElement | null;
    const el = ref.current;
    const first = el?.querySelector<HTMLElement>('[data-autofocus]') ?? el?.querySelector<HTMLElement>('button');
    first?.focus();
    return () => opener?.focus?.();
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && !busy) {
        e.stopPropagation();
        onCancel();
      }
      if (e.key === 'Tab' && ref.current) {
        const items = [...ref.current.querySelectorAll<HTMLElement>('button, [href], input, select')].filter(
          (x) => !x.hasAttribute('disabled'),
        );
        if (!items.length) return;
        const first = items[0];
        const last = items[items.length - 1];
        if (e.shiftKey && document.activeElement === first) {
          e.preventDefault();
          last.focus();
        } else if (!e.shiftKey && document.activeElement === last) {
          e.preventDefault();
          first.focus();
        }
      }
    };
    addEventListener('keydown', onKey, true);
    return () => removeEventListener('keydown', onKey, true);
  }, [onCancel, busy]);

  return (
    <div className="scrim">
      <div
        ref={ref}
        role="alertdialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={description ? descId : undefined}
        className={wide ? 'dialog wide' : 'dialog'}
      >
        <div className="dialog-body">
          <h2 id={titleId}>{title}</h2>
          {description && (
            <p id={descId} className={wide ? 'muted' : undefined} style={wide ? { marginTop: 4 } : undefined}>
              {description}
            </p>
          )}
        </div>
        {children}
        <div className="dialog-actions">{actions}</div>
      </div>
    </div>
  );
}
