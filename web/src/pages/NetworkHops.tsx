/**
 * 途中の無線機 on the network page: the devices on the way to the peer that つながりの確認 checks
 * after the peer and that the radio line may talk to (GET / PUT /settings/network, both roles).
 * The usual settings flow: the rows are a draft (kept while other screens are open, counted on the
 * menu), the device checks the draft as it changes (errors at the fields, the impact below), and
 * nothing is applied before a confirmation that lists every change. Applying does not touch the
 * transmission, so it is allowed while live.
 */
import { useEffect, useRef, useState, type ReactNode } from 'react';
import { ApiFailure, api } from '../api/client';
import { useDrafts, useDraftState, useLive } from '../api/live';
import type { CheckResult, FieldError as FieldErr, Hop, NetworkSettings, Role } from '../api/types';
import { Glyph } from '../components/Glyph';
import { Applied, ApplyDialog, DiscardDialog, FieldError, ImpactList, type PendingItem } from '../components/Settings';
import {
  hopErrorText,
  hopField,
  hopsApplyFailed,
  hopsConflict,
  hopsContext,
  hopsDone,
  hopsEmpty,
  hopsEmptyDraft,
  hopsFillIn,
  hopsFull,
  hopsHelp,
  hopsInvalid,
  hopsRebased,
  hopsRebasedWhileConfirming,
  hopsSameApplied,
} from '../i18n/ja';
import { timeOf } from '../lib/format';

/** The device accepts up to 4 (too_many above). */
const HOPS_MAX = 4;
/** focusNext: the 追加 button. */
const ADD = '#add';

/** One row as edited. `origin` is its index in the applied list it was taken from; null for a new row. */
interface HopRow {
  k: string;
  origin: number | null;
  label: string;
  address: string;
}

/** The draft and the applied list (and its version) it is compared with. */
interface HopDraft {
  base: number;
  baseHops: Hop[];
  rows: HopRow[];
}

type Change =
  | { kind: 'changed'; k: string; origin: number; from: Hop; to: Hop }
  | { kind: 'added'; k: string; to: Hop }
  | { kind: 'removed'; origin: number; from: Hop }
  | { kind: 'order'; from: Hop[]; to: Hop[] };

type Field = 'label' | 'address';

// Applied rows get keys from their place in that version, so that the first keystroke (which turns
// the applied list into a draft) keeps the same inputs and the focus.
const appliedKey = (version: number, i: number) => `a${version}-${i}`;
let newSeq = 0;
const newKey = () => `n${Date.now().toString(36)}-${++newSeq}`;

const rowsOf = (s: NetworkSettings): HopRow[] =>
  s.hops.map((h, i) => ({ k: appliedKey(s.version, i), origin: i, label: h.label, address: h.address }));
/** What is sent: both fields trimmed (the device trims the name and needs the address exact). */
const clean = (r: { label: string; address: string }): Hop => ({ label: r.label.trim(), address: r.address.trim() });
const same = (a: Hop | undefined, b: Hop | undefined) => !!a && !!b && a.label === b.label && a.address === b.address;
const line = (h: Hop) => `${h.label || '（名前なし）'}　${h.address || '（アドレスなし）'}`;

/**
 * A draft made against an older version (the list was applied elsewhere meanwhile), carried over to
 * the list applied now, so that only this screen's own edits stay changes:
 *  - each row is matched with an entry of the new list: one with the row's name and address as they
 *    are now (the other screen made the same edit), else the entry it was taken from, as it was,
 *    by its old address, or by its old name;
 *  - a matched row takes the new entry's name and address where this screen did not change them;
 *  - an entry this draft removes stays removed; an entry only the other screen added is kept;
 *  - a row taken from an entry the other screen removed goes too, unless this screen changed it;
 *    rows added here come last.
 */
function rebase(d: HopDraft, s: NetworkSettings): HopDraft {
  const used = new Set<number>();
  const take = (pred: (h: Hop) => boolean): number | null => {
    const j = s.hops.findIndex((h, k) => !used.has(k) && pred(h));
    if (j < 0) return null;
    used.add(j);
    return j;
  };
  const was = (r: HopRow) => (r.origin === null ? undefined : d.baseHops[r.origin]);
  const at: (number | null)[] = d.rows.map(() => null);
  const pass = (match: (r: HopRow) => ((h: Hop) => boolean) | null) =>
    d.rows.forEach((r, i) => {
      const m = at[i] === null ? match(r) : null;
      if (m) at[i] = take(m);
    });
  pass((r) => {
    const now = clean(r);
    return (h) => same(h, now);
  });
  pass((r) => {
    const w = was(r);
    return w ? (h) => same(h, w) : null;
  });
  pass((r) => {
    const w = was(r);
    return w ? (h) => h.address === w.address : null;
  });
  pass((r) => {
    const w = was(r);
    return w ? (h) => h.label === w.label : null;
  });
  // What this draft removes, found in the new list (perhaps edited there), stays removed.
  const kept = new Set(d.rows.map((r) => r.origin));
  d.baseHops.forEach((w, b) => {
    if (kept.has(b)) return;
    if (take((h) => same(h, w)) === null && take((h) => h.address === w.address) === null) take((h) => h.label === w.label);
  });
  const edited = (r: HopRow) => {
    const w = was(r);
    return !w || !same(clean(r), w);
  };
  const rows: HopRow[] = [];
  s.hops.forEach((h, j) => {
    const i = at.indexOf(j);
    if (i >= 0) {
      const r = d.rows[i];
      const w = was(r);
      const c = clean(r);
      rows.push({
        ...r,
        origin: j,
        label: !w || c.label !== w.label ? r.label : h.label,
        address: !w || c.address !== w.address ? r.address : h.address,
      });
    } else if (!used.has(j)) {
      rows.push({ k: appliedKey(s.version, j), origin: j, label: h.label, address: h.address });
    }
  });
  d.rows.forEach((r, i) => {
    if (at[i] === null && edited(r)) rows.push({ ...r, origin: null });
  });
  return { base: s.version, baseHops: s.hops, rows };
}

/** Every difference between the applied list and the rows: rows in their order, then the removed ones. */
function changesOf(applied: Hop[], rows: HopRow[]): Change[] {
  const out: Change[] = [];
  for (const r of rows) {
    const to = clean(r);
    if (r.origin === null || !applied[r.origin]) out.push({ kind: 'added', k: r.k, to });
    else if (!same(applied[r.origin], to)) out.push({ kind: 'changed', k: r.k, origin: r.origin, from: applied[r.origin], to });
  }
  const kept = rows.map((r) => r.origin).filter((o): o is number => o !== null);
  applied.forEach((h, i) => {
    if (!kept.includes(i)) out.push({ kind: 'removed', origin: i, from: h });
  });
  // Rows taken over from another screen's list can come in another order (never from this screen).
  if (kept.some((o, i) => i > 0 && o < kept[i - 1])) out.push({ kind: 'order', from: applied, to: rows.map(clean) });
  return out;
}

export function HopsSection({
  role,
  settings,
  onApplied,
  onReload,
}: {
  role: Role;
  /** The applied list; null while it is being read. */
  settings: NetworkSettings | null;
  onApplied: (s: NetworkSettings) => void;
  /** Read the applied list again (after a version conflict). */
  onReload: () => void;
}) {
  const { clockOffsetMs } = useLive();
  const { setCount } = useDrafts();
  const [stored, setStored] = useDraftState<HopDraft>('network', null);
  /** The device's answer to the last check, with the list it was for (null result: the check could not be made). */
  const [check, setCheck] = useState<{ key: string; hops: Hop[]; result: CheckResult | null } | null>(null);
  const [phase, setPhase] = useState<'edit' | 'confirm' | 'applying' | 'discard'>('edit');
  const [doneAt, setDoneAt] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ tone: 'err' | 'info'; text: string } | null>(null);
  /** Fields of new rows that were left once: their errors show from then on. */
  const [touched, setTouched] = useState<ReadonlySet<string>>(new Set());
  const [recheck, setRecheck] = useState(0);
  const checkSeq = useRef(0);
  const focusNext = useRef<string | null>(null);
  const addRef = useRef<HTMLButtonElement>(null);
  /** The confirmation was closed because the list changed elsewhere: the notice gets the focus. */
  const focusNotice = useRef(false);

  // The list was applied elsewhere: carry the draft over to the new list (rebase; dropped when none
  // of this screen's own edits is left). An open confirmation listed the changes against the old list and applying would now carry
  // the new version (no version_conflict any more), so it is closed: the user looks again first.
  // While applying, the request still carries the old version and the device decides.
  useEffect(() => {
    if (!settings || !stored || stored.base === settings.version) return;
    // A draft with nothing changed in it is no draft (it must not undo the other screen's list).
    if (changesOf(stored.baseHops, stored.rows).length === 0) return setStored(null);
    const next = rebase(stored, settings);
    const left = changesOf(settings.hops, next.rows).length > 0;
    setStored(left ? next : null);
    if (phase === 'confirm') {
      focusNotice.current = true;
      setPhase('edit');
    }
    if (!left) {
      if (phase !== 'applying') setNotice({ tone: 'info', text: hopsSameApplied });
    } else if (phase === 'confirm') setNotice({ tone: 'info', text: hopsRebasedWhileConfirming });
    else setNotice((n) => n ?? { tone: 'info', text: hopsRebased });
  }, [settings, stored, setStored, phase]);

  const draft = settings && stored && stored.base === settings.version ? stored : null;
  const rows = draft?.rows ?? (settings ? rowsOf(settings) : []);
  const changes = settings ? changesOf(settings.hops, rows) : [];
  const n = changes.length;
  const hops = rows.map(clean);
  const key = settings ? JSON.stringify([settings.version, hops]) : '';

  useEffect(() => setCount('network', n), [n, setCount]);

  // The device checks the draft 200 ms after the last change (and again when the confirmation opens).
  useEffect(() => {
    if (!settings || n === 0) {
      checkSeq.current += 1;
      setCheck(null);
      return;
    }
    const input = { version: settings.version, hops };
    const t = setTimeout(() => {
      const seq = ++checkSeq.current;
      api.network
        .check(input)
        .then((result) => seq === checkSeq.current && setCheck({ key, hops: input.hops, result }))
        .catch(() => seq === checkSeq.current && setCheck({ key, hops: input.hops, result: null }));
    }, 200);
    return () => clearTimeout(t);
    // `key` stands for settings.version and the cleaned rows.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, n, recheck]);

  // After the dialog has handed the focus back (its cleanup runs before this effect).
  useEffect(() => {
    if (phase !== 'edit' || !focusNotice.current) return;
    focusNotice.current = false;
    document.getElementById('hops-notice')?.focus();
  }, [phase]);

  // After the render: the name of a row just added, or 追加 after a row was removed (it may only
  // now be usable again).
  useEffect(() => {
    const f = focusNext.current;
    if (!f) return;
    focusNext.current = null;
    if (f === ADD) addRef.current?.focus();
    else document.getElementById(`hop-${f}-label`)?.focus();
  });

  if (!settings) {
    return (
      <SectionFrame>
        <p className="muted" style={{ margin: 0 }}>
          読み込み中…
        </p>
      </SectionFrame>
    );
  }

  const current = check?.key === key;
  const result = current ? (check?.result ?? null) : null;
  /** An error from the last check, if that row has not changed since. */
  const errorAt = (i: number, f: Field): FieldErr | undefined => {
    if (!check?.result || !same(check.hops[i], hops[i])) return undefined;
    return check.result.errors.find((e) => {
      const at = hopField(e.field);
      return at?.index === i && at.key === f;
    });
  };
  /** New rows say nothing about an empty field until it was left once (or applying was refused). */
  const quiet = (r: HopRow, f: Field) => r.origin === null && r[f].trim() === '' && !touched.has(`${r.k}.${f}`);
  const shown = (r: HopRow, i: number, f: Field) => (quiet(r, f) ? undefined : errorAt(i, f));
  const listError = current ? result?.errors.find((e) => e.field === 'hops') : undefined;
  /** duplicate: the number of the first earlier row with the same address. */
  const sameAs = (i: number) => {
    const j = hops.findIndex((h) => h.address === hops[i].address);
    return j >= 0 && j < i ? j + 1 : undefined;
  };

  const invalid = current && !!result && !result.ok;
  const anyShown = rows.some((r, i) => shown(r, i, 'label') || shown(r, i, 'address')) || !!listError;
  // Apply once the device has answered for this very draft (or could not be asked: it checks again on apply).
  const canApply = n > 0 && current && !invalid;
  const impacts = check?.result?.impacts ?? [];

  const edit = (f: (rows: HopRow[]) => HopRow[]) => {
    setDoneAt(null);
    setNotice(null);
    const base = draft ?? { base: settings.version, baseHops: settings.hops, rows: rowsOf(settings) };
    const next = f(base.rows);
    // Every change taken back: no draft is left (a list applied elsewhere later is then simply shown).
    setStored(changesOf(settings.hops, next).length > 0 ? { ...base, rows: next } : null);
  };
  const setField = (k: string, f: Field, value: string) => edit((rs) => rs.map((r) => (r.k === k ? { ...r, [f]: value } : r)));
  const touch = (k: string, f: Field) =>
    setTouched((t) => (t.has(`${k}.${f}`) ? t : new Set([...t, `${k}.${f}`])));
  const add = () => {
    const k = newKey();
    focusNext.current = k;
    edit((rs) => [...rs, { k, origin: null, label: '', address: '' }]);
  };
  const remove = (k: string) => {
    focusNext.current = ADD;
    edit((rs) => rs.filter((r) => r.k !== k));
  };
  /** Put a removed row back where it was among the rows taken from the applied list (focus: its name). */
  const restore = (o: number) => {
    const k = appliedKey(settings.version, o);
    focusNext.current = k;
    edit((rs) => {
      const h = settings.hops[o];
      const row: HopRow = { k, origin: o, label: h.label, address: h.address };
      const at = rs.findIndex((r) => r.origin !== null && r.origin > o);
      if (at >= 0) return [...rs.slice(0, at), row, ...rs.slice(at)];
      let last = -1;
      rs.forEach((r, i) => {
        if (r.origin !== null && r.origin < o) last = i;
      });
      return [...rs.slice(0, last + 1), row, ...rs.slice(last + 1)];
    });
  };
  const revert = (c: Change) => {
    switch (c.kind) {
      case 'changed':
        // 元に戻す goes away with the change: the focus moves to the row's name.
        focusNext.current = c.k;
        return edit((rs) => rs.map((r) => (r.k === c.k ? { ...r, label: c.from.label, address: c.from.address } : r)));
      case 'added':
        return remove(c.k);
      case 'removed':
        return restore(c.origin);
      case 'order':
        return edit((rs) => [...rs].sort((a, b) => (a.origin ?? Infinity) - (b.origin ?? Infinity)));
    }
  };

  const items: PendingItem[] = changes.map((c, i) => {
    const back = () => revert(c);
    switch (c.kind) {
      case 'changed': {
        const renamed = c.from.label !== c.to.label;
        const moved = c.from.address !== c.to.address;
        return {
          key: `c-${c.k}`,
          label: c.from.label,
          from: renamed && moved ? line(c.from) : renamed ? c.from.label : c.from.address,
          to: renamed && moved ? line(c.to) : renamed ? c.to.label || '（名前なし）' : c.to.address || '（アドレスなし）',
          revert: back,
        };
      }
      case 'added':
        return { key: `a-${c.k}`, label: '追加', from: 'なし', to: line(c.to), revert: back };
      case 'removed':
        return { key: `r-${c.origin}-${i}`, label: '削除', from: line(c.from), to: 'なし', revert: back };
      case 'order':
        return {
          key: 'order',
          label: '並び順',
          from: c.from.map((h) => h.label).join('、'),
          to: c.to.map((h) => h.label).join('、'),
          revert: back,
        };
    }
  });
  const changeOf = (r: HopRow) =>
    changes.find((c): c is Extract<Change, { kind: 'changed' | 'added' }> => (c.kind === 'changed' || c.kind === 'added') && c.k === r.k);
  const removed = changes.filter((c): c is Extract<Change, { kind: 'removed' }> => c.kind === 'removed');

  const apply = async () => {
    setPhase('applying');
    try {
      const s = await api.network.apply({ version: settings.version, hops });
      setStored(null);
      setTouched(new Set());
      setCheck(null);
      setNotice(null);
      setDoneAt(timeOf(Date.now() + clockOffsetMs));
      setPhase('edit');
      onApplied(s);
    } catch (e) {
      setPhase('edit');
      const body = e instanceof ApiFailure ? e.body : null;
      if (body?.error.code === 'version_conflict') {
        setNotice({ tone: 'err', text: hopsConflict });
        onReload();
      } else if (body?.error.code === 'invalid' && body.error.errors?.length) {
        // Show every error now, the empty fields of new rows included.
        setTouched(new Set(rows.flatMap((r) => [`${r.k}.label`, `${r.k}.address`])));
        setCheck({ key, hops, result: { ok: false, errors: body.error.errors, impacts } });
        setNotice({ tone: 'err', text: hopsInvalid });
      } else {
        setNotice({ tone: 'err', text: hopsApplyFailed });
      }
    }
  };

  return (
    <SectionFrame>
      {rows.length > 0 ? (
        <div className="hops">
          <div className="hops-head" aria-hidden="true">
            <span />
            <span>名前</span>
            <span>アドレス</span>
            <span />
          </div>
          {rows.map((r, i) => {
            const c = changeOf(r);
            const labelErr = shown(r, i, 'label');
            const addrErr = shown(r, i, 'address');
            const bad = !!labelErr || !!addrErr;
            const name = r.label.trim() || `無線機 ${i + 1}`;
            const cls = ['hop', bad ? 'invalid' : c ? 'changed' : ''].filter(Boolean).join(' ');
            const lid = `hop-${r.k}-label`;
            const aid = `hop-${r.k}-address`;
            return (
              <div key={r.k} className={cls} role="group" aria-label={`無線機 ${i + 1}`}>
                <div className="hop-no">
                  <span aria-hidden="true">{i + 1}</span>
                  {c && <span className="chip-changed">{c.kind === 'added' ? '追加' : '変更'}</span>}
                </div>
                <div className="hop-label">
                  <label htmlFor={lid} className="hop-lab">
                    名前
                  </label>
                  <input
                    id={lid}
                    type="text"
                    autoComplete="off"
                    className={c && (c.kind === 'added' || c.from.label !== c.to.label) ? 'input changed' : 'input'}
                    value={r.label}
                    placeholder="例 無線機（送信機側）"
                    aria-invalid={!!labelErr}
                    aria-describedby={labelErr ? `${lid}-e` : undefined}
                    onChange={(e) => setField(r.k, 'label', e.target.value)}
                    onBlur={() => touch(r.k, 'label')}
                  />
                  {labelErr && <FieldError id={`${lid}-e`} text={hopErrorText(role, labelErr, r.label)} />}
                </div>
                <div className="hop-address">
                  <label htmlFor={aid} className="hop-lab">
                    アドレス
                  </label>
                  <input
                    id={aid}
                    type="text"
                    inputMode="decimal"
                    autoComplete="off"
                    spellCheck={false}
                    className={c && (c.kind === 'added' || c.from.address !== c.to.address) ? 'input changed' : 'input'}
                    value={r.address}
                    placeholder="例 192.168.0.65"
                    aria-invalid={!!addrErr}
                    aria-describedby={addrErr ? `${aid}-e` : undefined}
                    onChange={(e) => setField(r.k, 'address', e.target.value)}
                    onBlur={() => touch(r.k, 'address')}
                  />
                  {addrErr && <FieldError id={`${aid}-e`} text={hopErrorText(role, addrErr, r.address, sameAs(i))} />}
                </div>
                <div className="hop-act">
                  <button type="button" className="btn btn-sm" aria-label={`${name}を削除`} onClick={() => remove(r.k)}>
                    削除
                  </button>
                </div>
                {c?.kind === 'changed' && (
                  <div className="hop-applied">
                    <Applied value={line(c.from)} onRevert={() => revert(c)} />
                  </div>
                )}
              </div>
            );
          })}
        </div>
      ) : (
        <p className="hop-empty">{n > 0 ? hopsEmptyDraft(role) : hopsEmpty(role)}</p>
      )}

      {removed.length > 0 && (
        <ul className="hop-gone" aria-label="削除する無線機">
          {removed.map((c) => (
            <li key={c.origin}>
              <span className="chip-outline">削除</span>
              <s>{line(c.from)}</s>
              <button type="button" className="linkbtn" aria-label={`${c.from.label}を元に戻す`} onClick={() => revert(c)}>
                元に戻す
              </button>
            </li>
          ))}
        </ul>
      )}

      <div className="nt-actions hop-add">
        <button ref={addRef} type="button" className="btn" onClick={add} disabled={rows.length >= HOPS_MAX}>
          追加
        </button>
        {rows.length >= HOPS_MAX && !listError && <span className="muted">{hopsFull(HOPS_MAX)}</span>}
      </div>
      {listError && <FieldError text={hopErrorText(role, listError)} />}

      {n > 0 ? (
        <div className="hop-pending">
          <p className="hop-pending-title">未適用の変更が {n} 件あります</p>
          {impacts.length > 0 && (
            <>
              <h3>適用したときの影響</h3>
              <ImpactList impacts={impacts} />
            </>
          )}
          {invalid && (
            <p className="hop-invalid">{anyShown ? '入力に誤りがある項目があります。直すと適用できます。' : hopsFillIn}</p>
          )}
          <div className="pending-actions">
            <button type="button" className="btn" onClick={() => setPhase('discard')}>
              すべて取り消す
            </button>
            <button
              type="button"
              className="btn btn-primary"
              disabled={!canApply}
              onClick={() => {
                if (!canApply) return;
                setRecheck((x) => x + 1);
                setPhase('confirm');
              }}
            >
              内容を確認して適用…
            </button>
          </div>
        </div>
      ) : (
        doneAt && (
          <p className="nt-line nt-used" role="status">
            <Glyph tone="ok" />
            <span>{hopsDone(doneAt)}</span>
          </p>
        )
      )}
      {notice && (
        <p
          id="hops-notice"
          tabIndex={-1}
          className={notice.tone === 'err' ? 'nt-line nt-problem' : 'nt-line'}
          role={notice.tone === 'err' ? 'alert' : 'status'}
        >
          <Glyph tone={notice.tone} />
          <span>{notice.text}</span>
        </p>
      )}

      {(phase === 'confirm' || phase === 'applying') && n > 0 && (
        <ApplyDialog
          items={items}
          impacts={impacts}
          context={hopsContext}
          applying={phase === 'applying'}
          applyingText="適用しています…"
          onCancel={() => setPhase('edit')}
          onApply={apply}
        />
      )}
      {phase === 'discard' && n > 0 && (
        <DiscardDialog
          n={n}
          onCancel={() => setPhase('edit')}
          onConfirm={() => {
            setStored(null);
            setTouched(new Set());
            setNotice(null);
            setPhase('edit');
          }}
        />
      )}
    </SectionFrame>
  );
}

function SectionFrame({ children }: { children: ReactNode }) {
  return (
    <section aria-labelledby="sec-hops" className="nt-sec">
      <h2 id="sec-hops" className="sec-title">
        途中の無線機
      </h2>
      <div className="nt-body">
        <p className="nt-help">{hopsHelp}</p>
        {children}
      </div>
    </section>
  );
}
