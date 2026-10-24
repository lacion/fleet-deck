// BG-2 (design `docs/v2/design/board-gaps.md` §3.2, §3.6, §3.7): the dialogs and the section of the
// installer repair on the Omarchy card. The `*View` components show what they are given; the
// dialogs read their plan once, hold the selection, and hand the choice to the card.
import { useEffect, useId, useState } from 'react';
import { Dialog } from '../integrations/Dialog';
import type { RepairStatusView } from './omarchyRepair';
import {
  CONFLICT_TEXT,
  canReplace,
  type FileLine,
  fileName,
  hasKeptCopy,
  type InstallPlanView,
  journalDate,
  journalFileLines,
  journalStateText,
  LOADING_PLAN_TEXT,
  NO_REPAIRS_TEXT,
  NO_REPLACE_TEXT,
  NOTHING_RESTORABLE_TEXT,
  type OmarchyAssetId,
  RECORDED_EDIT_TEXT,
  REPAIRS_ERROR_TEXT,
  REPAIRS_TITLE,
  REVIEW_CONFIRM_BUTTON,
  REVIEW_CONFIRMATION,
  REVIEW_TITLE,
  RESTORE_BUTTON,
  RESTORE_CONFIRM_BUTTON,
  RESTORE_CONFIRMATION,
  RESTORE_TITLE,
  type RestorePlanView,
  RESUME_BUTTON,
  restorable,
  restoreActionText,
  restorableAssets,
  selectedEdits,
  UNSAFE_TEXT,
} from './omarchyRepair';

/** A plan on its way to a dialog. */
export type PlanLoad<T> =
  | { readonly kind: 'loading' }
  | { readonly kind: 'error'; readonly text: string }
  | { readonly kind: 'ready'; readonly plan: T };

/** The repairs of the card: not read yet, not offered by the station, failed, or read. */
export type RepairsState =
  | { readonly kind: 'idle' }
  | { readonly kind: 'unsupported' }
  | { readonly kind: 'error' }
  | { readonly kind: 'ready'; readonly journals: RepairStatusView['journals'] };

/** The note of a file: its state, its kept copy, and a later edit that is kept aside. */
export function FileLines({ lines }: { readonly lines: readonly FileLine[] }) {
  if (lines.length === 0) return null;
  return (
    <ul className="fd-omarchy-files">
      {lines.map((l) => (
        <li key={`${l.line}/${l.kept}/${l.edit ?? ''}`}>
          <span>{l.line}</span>
          <span className="fd-note">{l.kept}</span>
          {l.edit === undefined ? null : <span className="fd-note">{l.edit}</span>}
        </li>
      ))}
    </ul>
  );
}

function PlanStatus({ load }: { readonly load: PlanLoad<unknown> }) {
  if (load.kind === 'loading') return <p role="status">{LOADING_PLAN_TEXT}</p>;
  if (load.kind === 'error') return <p role="alert">{load.text}</p>;
  return null;
}

// ── the review of a replacement ───────────────────────────────────────────────────────────────

type ReviewViewProps = {
  readonly load: PlanLoad<InstallPlanView>;
  /** The recorded edits that the owner selected. */
  readonly checked: ReadonlySet<OmarchyAssetId>;
  readonly onToggle: (assetId: OmarchyAssetId) => void;
  readonly onConfirm: () => void;
  readonly onCancel: () => void;
  /** An action runs: the confirm button is off. */
  readonly busy?: boolean;
};

export function ReviewDialogView({
  load,
  checked,
  onToggle,
  onConfirm,
  onCancel,
  busy = false,
}: ReviewViewProps) {
  const base = useId();
  const plan = load.kind === 'ready' ? load.plan : null;
  const files = plan?.files ?? [];
  return (
    <Dialog title={REVIEW_TITLE} onClose={onCancel}>
      <div className="fd-set-dialogform">
        <PlanStatus load={load} />
        {plan !== null && files.length === 0 ? <p>{NO_REPLACE_TEXT}</p> : null}
        {files.length === 0 ? null : (
          <ul className="fd-omarchy-files">
            {files.map((f, i) => {
              const id = `${base}-${i}`;
              const recorded = f.classification === 'recorded_edit';
              const note = !f.replaceable
                ? UNSAFE_TEXT
                : recorded
                  ? RECORDED_EDIT_TEXT
                  : CONFLICT_TEXT;
              return (
                <li key={f.asset_id}>
                  <label>
                    <input
                      type="checkbox"
                      checked={recorded ? checked.has(f.asset_id) : true}
                      disabled={!recorded || !f.replaceable}
                      aria-describedby={id}
                      onChange={() => onToggle(f.asset_id)}
                    />{' '}
                    <span className="fd-omarchy-path">{f.path}</span>
                  </label>
                  <span id={id} className="fd-note">
                    {note}
                  </span>
                </li>
              );
            })}
          </ul>
        )}
        {plan !== null && files.length > 0 ? <p>{REVIEW_CONFIRMATION}</p> : null}
        <div className="fd-formactions">
          <button type="button" className="fd-ghost" onClick={onCancel}>
            Cancel
          </button>
          <button
            type="button"
            className="fd-primary"
            disabled={busy || plan === null || !canReplace(plan)}
            onClick={onConfirm}
          >
            {REVIEW_CONFIRM_BUTTON}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

type ReviewProps = {
  /** Reads the plan once (through the gate of the card). */
  readonly load: () => Promise<PlanLoad<InstallPlanView>>;
  readonly busy?: boolean;
  readonly onConfirm: (plan: InstallPlanView, edits: OmarchyAssetId[]) => void;
  readonly onCancel: () => void;
};

export function ReviewDialog({ load, busy, onConfirm, onCancel }: ReviewProps) {
  const [state, setState] = useState<PlanLoad<InstallPlanView>>({ kind: 'loading' });
  const [checked, setChecked] = useState<ReadonlySet<OmarchyAssetId>>(new Set());
  useEffect(() => {
    let dead = false;
    void load().then((next) => {
      if (!dead) setState(next);
    });
    return () => {
      dead = true;
    };
  }, [load]);
  return (
    <ReviewDialogView
      load={state}
      busy={busy ?? false}
      checked={checked}
      onToggle={(id) =>
        setChecked((prev) => {
          const next = new Set(prev);
          if (!next.delete(id)) next.add(id);
          return next;
        })
      }
      onConfirm={() => {
        if (state.kind === 'ready') onConfirm(state.plan, selectedEdits(state.plan, checked));
      }}
      onCancel={onCancel}
    />
  );
}

// ── the restore of kept copies ────────────────────────────────────────────────────────────────

type RestoreViewProps = {
  readonly load: PlanLoad<RestorePlanView>;
  readonly checked: ReadonlySet<OmarchyAssetId>;
  readonly onToggle: (assetId: OmarchyAssetId) => void;
  readonly onConfirm: () => void;
  readonly onCancel: () => void;
  /** An action runs: the confirm button is off. */
  readonly busy?: boolean;
};

export function RestoreDialogView({
  load,
  checked,
  onToggle,
  onConfirm,
  onCancel,
  busy = false,
}: RestoreViewProps) {
  const plan = load.kind === 'ready' ? load.plan : null;
  const files = plan?.files ?? [];
  const none = plan !== null && restorableAssets(plan).length === 0;
  const any = plan !== null && restorableAssets(plan).some((id) => checked.has(id));
  return (
    <Dialog title={RESTORE_TITLE} onClose={onCancel}>
      <div className="fd-set-dialogform">
        <PlanStatus load={load} />
        {none ? <p>{NOTHING_RESTORABLE_TEXT}</p> : null}
        {files.length === 0 ? null : (
          <ul className="fd-omarchy-files">
            {files.map((f) => {
              const name = fileName(f.asset_id, f.path);
              const text = restoreActionText(f.action);
              return (
                <li key={f.asset_id}>
                  {restorable(f.action) ? (
                    <>
                      <label>
                        <input
                          type="checkbox"
                          checked={checked.has(f.asset_id)}
                          onChange={() => onToggle(f.asset_id)}
                        />{' '}
                        <span>{name}</span>
                      </label>
                      <span className="fd-note">{text}</span>
                    </>
                  ) : (
                    <span>{`${name}: ${text}`}</span>
                  )}
                </li>
              );
            })}
          </ul>
        )}
        {plan !== null && !none ? <p>{RESTORE_CONFIRMATION}</p> : null}
        <div className="fd-formactions">
          <button type="button" className="fd-ghost" onClick={onCancel}>
            Cancel
          </button>
          <button type="button" className="fd-primary" disabled={busy || !any} onClick={onConfirm}>
            {RESTORE_CONFIRM_BUTTON}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

type RestoreProps = {
  readonly load: () => Promise<PlanLoad<RestorePlanView>>;
  readonly busy?: boolean;
  readonly onConfirm: (plan: RestorePlanView, assets: OmarchyAssetId[]) => void;
  readonly onCancel: () => void;
};

export function RestoreDialog({ load, busy, onConfirm, onCancel }: RestoreProps) {
  const [state, setState] = useState<PlanLoad<RestorePlanView>>({ kind: 'loading' });
  const [checked, setChecked] = useState<ReadonlySet<OmarchyAssetId>>(new Set());
  useEffect(() => {
    let dead = false;
    void load().then((next) => {
      if (!dead) setState(next);
    });
    return () => {
      dead = true;
    };
  }, [load]);
  return (
    <RestoreDialogView
      load={state}
      busy={busy ?? false}
      checked={checked}
      onToggle={(id) =>
        setChecked((prev) => {
          const next = new Set(prev);
          if (!next.delete(id)) next.add(id);
          return next;
        })
      }
      onConfirm={() => {
        if (state.kind !== 'ready') return;
        // The plan order, and only the files that a restore can change.
        const chosen = restorableAssets(state.plan).filter((id) => checked.has(id));
        if (chosen.length > 0) onConfirm(state.plan, chosen);
      }}
      onCancel={onCancel}
    />
  );
}

// ── the Repairs section ───────────────────────────────────────────────────────────────────────

type SectionProps = {
  readonly state: RepairsState;
  readonly open: boolean;
  /** An action runs: the buttons are off. */
  readonly working: boolean;
  readonly onToggle: () => void;
  readonly onResume: (repairId: string) => void;
  readonly onRestore: (repairId: string) => void;
};

function SectionBody({
  state,
  working,
  onResume,
  onRestore,
}: Omit<SectionProps, 'open' | 'onToggle'>) {
  if (state.kind === 'error') return <p role="alert">{REPAIRS_ERROR_TEXT}</p>;
  if (state.kind === 'idle' || state.kind === 'unsupported') return null;
  if (state.journals.length === 0) return <p className="fd-note">{NO_REPAIRS_TEXT}</p>;
  return (
    <ul className="fd-omarchy-repairlist">
      {state.journals.map((j) => (
        <li key={j.repair_id}>
          <p>
            <span>{journalDate(j.created_at)}</span> <strong>{journalStateText(j.state)}</strong>
          </p>
          <FileLines lines={journalFileLines(j)} />
          <div className="fd-set-bar-actions">
            {j.state === 'incomplete' ? (
              <button
                type="button"
                className="fd-sm"
                disabled={working}
                onClick={() => onResume(j.repair_id)}
              >
                {RESUME_BUTTON}
              </button>
            ) : null}
            {hasKeptCopy(j) ? (
              <button
                type="button"
                className="fd-sm"
                disabled={working}
                onClick={() => onRestore(j.repair_id)}
              >
                {RESTORE_BUTTON}
              </button>
            ) : null}
          </div>
        </li>
      ))}
    </ul>
  );
}

/** The collapsed section **Repairs**. No section before the first read or without the feature. */
export function RepairsSection(props: SectionProps) {
  if (props.state.kind === 'unsupported' || props.state.kind === 'idle') return null;
  const { open, onToggle } = props;
  return (
    <div className="fd-omarchy-repairs">
      <h3>
        <button
          type="button"
          className="fd-omarchy-disclosure"
          aria-expanded={open}
          onClick={onToggle}
        >
          {REPAIRS_TITLE}
        </button>
      </h3>
      {open ? <SectionBody {...props} /> : null}
    </div>
  );
}
