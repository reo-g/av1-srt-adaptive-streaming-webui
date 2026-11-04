import type { ReactNode } from 'react';
import type { Impact } from '../api/types';
import { impactText, visibleImpacts } from '../i18n/ja';
import { href } from '../lib/router';
import { AlertDialog } from './Dialog';
import { Glyph } from './Glyph';

/** One unapplied change: what it was, what it will be. */
export interface PendingItem {
  key: string;
  label: string;
  from: string;
  to: string;
  revert: () => void;
}

export const ChangedChip = ({ show }: { show: boolean }) => (show ? <span className="chip-changed">変更</span> : null);

/** Row of a settings form: label column on the left, control on the right. */
export function Row({
  label,
  labelFor,
  labelId,
  hint,
  changed,
  invalid,
  top,
  padLabel,
  children,
}: {
  label: ReactNode;
  labelFor?: string;
  labelId?: string;
  hint?: ReactNode;
  changed?: boolean;
  invalid?: boolean;
  top?: boolean;
  padLabel?: boolean;
  children: ReactNode;
}) {
  const cls = ['row', top || changed || invalid ? 'top' : '', invalid ? 'invalid' : changed ? 'changed' : '']
    .filter(Boolean)
    .join(' ');
  const name = (
    <>
      {label}
      <ChangedChip show={!!changed} />
    </>
  );
  return (
    <div className={cls}>
      <div className={padLabel ? 'row-label pad' : 'row-label'}>
        {labelFor ? (
          <label htmlFor={labelFor} className="name">
            {name}
          </label>
        ) : (
          <span id={labelId} className="name">
            {name}
          </span>
        )}
        {hint && <div className="hint">{hint}</div>}
      </div>
      <div className="row-control">{children}</div>
    </div>
  );
}

/** "適用中 …  元に戻す" under a changed field. */
export const Applied = ({ value, onRevert }: { value: string; onRevert: () => void }) => (
  <div className="applied">
    適用中　{value}
    <button type="button" className="linkbtn" onClick={onRevert}>
      元に戻す
    </button>
  </div>
);

export const FieldError = ({ id, text }: { id?: string; text: string }) => (
  <div id={id} className="field-error">
    <Glyph tone="err" />
    {text}
  </div>
);

const impactTone = (i: Impact) => (i.level === 'warn' ? 'warn' : i.level === 'ok' ? 'ok' : 'info');

/** `stopped`: the device is stopped (impacts on what is shown now then take effect at the next start). */
export function ImpactList({ impacts, large, stopped }: { impacts: Impact[]; large?: boolean; stopped?: boolean }) {
  return (
    <ul className="impacts">
      {visibleImpacts(impacts).map((m) => (
        <li key={m.code} style={large ? { fontSize: 14 } : undefined}>
          <Glyph tone={impactTone(m)} />
          <span>{impactText(m, { stopped })}</span>
        </li>
      ))}
    </ul>
  );
}

/** Right-hand panel listing unapplied changes, their impact and the apply/discard buttons. */
export function PendingPanel({
  items,
  impacts,
  invalid,
  onDiscard,
  onApply,
  doneAt,
  doneNote,
  emptyNote,
  emptyLink,
  stopped,
}: {
  items: PendingItem[];
  impacts: Impact[];
  invalid: boolean;
  onDiscard: () => void;
  onApply: () => void;
  doneAt: string | null;
  doneNote: string;
  emptyNote: ReactNode;
  emptyLink?: ReactNode;
  stopped?: boolean;
}) {
  const n = items.length;
  return (
    <aside className="pending" aria-labelledby="pending-title">
      <div className="pending-inner">
        <h2 id="pending-title">変更内容</h2>
        {n > 0 ? (
          <div>
            <p style={{ margin: '4px 0 0', fontWeight: 600 }}>未適用の変更が {n} 件あります</p>
            <ul className="pending-list">
              {items.map((p) => (
                <li key={p.key}>
                  <span style={{ fontWeight: 600 }}>{p.label}</span>
                  <button type="button" className="linkbtn linkbtn-plain" onClick={p.revert}>
                    元に戻す
                  </button>
                  <span className="fromto">
                    <span className="muted">{p.from}</span>
                    <span aria-label="から" className="arrow">
                      →
                    </span>
                    <span style={{ fontWeight: 600 }}>{p.to}</span>
                  </span>
                </li>
              ))}
            </ul>
            {impacts.length > 0 && (
              <>
                <h3>適用したときの影響</h3>
                <ImpactList impacts={impacts} stopped={stopped} />
              </>
            )}
            {invalid && (
              <p style={{ margin: '14px 0 0', fontSize: 13, fontWeight: 600, color: 'var(--err-text)' }}>
                入力に誤りがある項目があります。直すと適用できます。
              </p>
            )}
            <div className="pending-actions">
              <button type="button" className="btn" onClick={onDiscard}>
                すべて取り消す
              </button>
              <button type="button" className="btn btn-primary" onClick={onApply} disabled={invalid}>
                内容を確認して適用…
              </button>
            </div>
          </div>
        ) : doneAt ? (
          <div role="status" style={{ display: 'flex', gap: 10, alignItems: 'flex-start', marginTop: 12 }}>
            <span style={{ marginTop: 5 }}>
              <Glyph tone="ok" size={12} />
            </span>
            <div>
              <div style={{ fontWeight: 600, color: 'var(--ok)' }}>{doneAt} に適用しました</div>
              <div style={{ fontSize: 13 }} className="muted">
                {doneNote}
              </div>
              <a href={href('ops')} style={{ fontSize: 13 }}>
                運用画面で確認する
              </a>
            </div>
          </div>
        ) : (
          <div>
            <p style={{ margin: '8px 0 0' }} className="muted">
              {emptyNote}
            </p>
            {emptyLink ?? (
              <a href={href('ops')} style={{ display: 'inline-block', marginTop: 8, fontSize: 13, fontWeight: 600 }}>
                運用画面へ戻る
              </a>
            )}
          </div>
        )}
      </div>
    </aside>
  );
}

export function ApplyDialog({
  items,
  impacts,
  context,
  applying,
  applyingText,
  onCancel,
  onApply,
  stopped,
}: {
  items: PendingItem[];
  impacts: Impact[];
  context: string;
  applying: boolean;
  applyingText: string;
  onCancel: () => void;
  onApply: () => void;
  stopped?: boolean;
}) {
  return (
    <AlertDialog
      wide
      busy={applying}
      title="変更を適用しますか"
      description={context}
      onCancel={onCancel}
      actions={
        applying ? (
          <div role="status" style={{ display: 'flex', alignItems: 'center', minHeight: 44, fontWeight: 600 }}>
            {applyingText}
          </div>
        ) : (
          <>
            <button type="button" className="btn" onClick={onCancel} data-autofocus>
              戻って編集する
            </button>
            <button type="button" className="btn btn-primary" onClick={onApply} style={{ padding: '0 22px' }}>
              適用する
            </button>
          </>
        )
      }
    >
      <div className="diff-table">
        <div className="diff-row head">
          <span>項目</span>
          <span>適用中</span>
          <span>変更後</span>
        </div>
        {items.map((p) => (
          <div className="diff-row" key={p.key}>
            <span style={{ fontWeight: 600 }}>{p.label}</span>
            <span className="muted">{p.from}</span>
            <span style={{ fontWeight: 600 }}>{p.to}</span>
          </div>
        ))}
      </div>
      {impacts.length > 0 && (
        <div style={{ padding: '16px 24px 4px' }}>
          <h3 style={{ margin: '0 0 8px', fontSize: 14, fontWeight: 600 }}>適用すると</h3>
          <ImpactList impacts={impacts} large stopped={stopped} />
        </div>
      )}
    </AlertDialog>
  );
}

export function DiscardDialog({ n, onCancel, onConfirm }: { n: number; onCancel: () => void; onConfirm: () => void }) {
  return (
    <AlertDialog
      title={`未適用の変更 ${n} 件を取り消しますか`}
      description="入力した値は破棄され、適用中の値に戻ります。"
      onCancel={onCancel}
      actions={
        <>
          <button type="button" className="btn" onClick={onCancel} data-autofocus>
            戻る
          </button>
          <button type="button" className="btn btn-danger" onClick={onConfirm}>
            取り消す
          </button>
        </>
      }
    />
  );
}
