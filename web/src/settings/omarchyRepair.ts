// BG-2 (design `docs/v2/design/board-gaps.md` §3.2, §3.6, §3.7): the answers, the texts and the
// pure helpers of the installer repair on the Omarchy card. The deck sends the plans and the
// repair status; the board validates each shape and shows only the fixed texts below.
import type {
  InstallPlanView,
  OmarchyStatus,
  JournalState,
  ObservedState,
  OmarchyAction,
  OmarchyActParams,
  OmarchyAssetId,
  PlanClassification,
  RepairFile,
  RepairFileState,
  RepairJournalView,
  RepairResult,
  RepairStatusView,
  RestoreAction,
  RestorePlanView,
} from '../gen';
import type { DeckIo } from './http';

export type {
  InstallPlanView,
  JournalState,
  ObservedState,
  OmarchyAssetId,
  PlanClassification,
  RepairFile,
  RepairFileState,
  RepairJournalView,
  RepairResult,
  RepairStatusView,
  RestoreAction,
  RestorePlanView,
};

/** The optional fields of `omarchy.act` for the three repair actions. */
export type ActExtra = Omit<OmarchyActParams, 'action' | 'elevated'>;

export const INVALID_ANSWER = 'The deck sent an invalid answer.';

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

const invalid = (): Error => new Error(INVALID_ANSWER);

// A `Record` over the whole union: a new value in the generated type fails to compile here.
const ASSET_IDS: Readonly<Record<OmarchyAssetId, true>> = {
  fleet_plugin_manifest: true,
  fleet_plugin_widget: true,
  fleet_plugin_panel: true,
  fleet_plugin_service: true,
  fleet_plugin_model: true,
  fleet_plugin_i18n: true,
  hook_post_boot: true,
  hook_post_update: true,
  hook_theme_set: true,
  theme_template: true,
  fleet_bindings: true,
};

const CLASSIFICATIONS: Readonly<Record<PlanClassification, true>> = {
  unrecorded_conflict: true,
  recorded_edit: true,
};

const JOURNAL_STATES: Readonly<Record<JournalState, string>> = {
  complete: 'Complete',
  incomplete: 'Incomplete',
  restored: 'Restored',
};

/** The owner text of what the files on disk say about one file of a repair. */
const OBSERVED_TEXTS: Readonly<Record<ObservedState, string>> = {
  nothing_moved: 'Not changed',
  backed_up: 'Kept aside, not replaced yet',
  published: 'Replaced',
  restored: 'Restored',
  conflict: 'Changed after the repair',
  incomplete: 'Incomplete',
};

/** The owner text of what a command did to one file. */
const FILE_STATE_TEXTS: Readonly<Record<RepairFileState, string>> = {
  unchanged: 'Not changed',
  backed_up: 'Kept aside, not replaced yet',
  replaced: 'Replaced',
  restored: 'Restored',
};

/** The owner text of what a restore does to one file. */
const RESTORE_ACTION_TEXTS: Readonly<Record<RestoreAction, string>> = {
  copy_backup: 'Copy the kept file back',
  move_aside_then_copy_backup: 'Keep the current file aside, then copy the kept file back',
  remove_published: 'Remove the file that Fleet Deck added',
  nothing: 'Nothing to restore',
  stop_restore_conflict: 'Changed after the repair: not restored',
  stop_repair_incomplete: 'Incomplete: not restored',
};

const has = (table: object, key: unknown): boolean =>
  typeof key === 'string' && Object.hasOwn(table, key);

function assetIdOf(v: unknown): OmarchyAssetId {
  if (!has(ASSET_IDS, v)) throw invalid();
  return v as OmarchyAssetId;
}

const stringOf = (v: unknown): string => {
  if (typeof v !== 'string') throw invalid();
  return v;
};

const numberOf = (v: unknown): number => {
  if (typeof v !== 'number' || !Number.isFinite(v)) throw invalid();
  return v;
};

const stringOrNull = (v: unknown): string | null => {
  if (v === undefined || v === null) return null;
  return stringOf(v);
};

const rows = (v: unknown): Record<string, unknown>[] => {
  if (!Array.isArray(v)) throw invalid();
  return v.map((row: unknown) => {
    if (!record(row)) throw invalid();
    return row;
  });
};

/** The answer of `GET .../omarchy/install-plan`. */
export function installPlanOf(v: unknown): InstallPlanView {
  if (!record(v)) throw invalid();
  return {
    plan_id: stringOf(v['plan_id']),
    expires_at: numberOf(v['expires_at']),
    files: rows(v['files']).map((f) => {
      if (!has(CLASSIFICATIONS, f['classification']) || typeof f['replaceable'] !== 'boolean') {
        throw invalid();
      }
      return {
        asset_id: assetIdOf(f['asset_id']),
        path: stringOf(f['path']),
        classification: f['classification'] as PlanClassification,
        replaceable: f['replaceable'],
      };
    }),
    blocked: stringOrNull(v['blocked']),
  };
}

/** The answer of `GET .../omarchy/repairs`. */
export function repairStatusOf(v: unknown): RepairStatusView {
  if (!record(v)) throw invalid();
  return {
    journals: rows(v['journals']).map((j) => {
      if (!has(JOURNAL_STATES, j['state'])) throw invalid();
      return {
        repair_id: stringOf(j['repair_id']),
        created_at: numberOf(j['created_at']),
        state: j['state'] as JournalState,
        files: rows(j['files']).map((f) => {
          if (!has(OBSERVED_TEXTS, f['state'])) throw invalid();
          return {
            asset_id: assetIdOf(f['asset_id']),
            state: f['state'] as ObservedState,
            backup_path: stringOrNull(f['backup_path']),
          };
        }),
      };
    }),
  };
}

/** The answer of `GET .../omarchy/repairs/{id}/restore-plan`. */
export function restorePlanOf(v: unknown): RestorePlanView {
  if (!record(v)) throw invalid();
  return {
    plan_id: stringOf(v['plan_id']),
    expires_at: numberOf(v['expires_at']),
    repair_id: stringOf(v['repair_id']),
    files: rows(v['files']).map((f) => {
      if (!has(RESTORE_ACTION_TEXTS, f['action'])) throw invalid();
      return {
        asset_id: assetIdOf(f['asset_id']),
        path: stringOf(f['path']),
        action: f['action'] as RestoreAction,
      };
    }),
  };
}

/** The `repair` part of an `omarchy.act` answer. */
export function repairResultOf(v: unknown): RepairResult {
  if (!record(v)) throw invalid();
  return {
    code: stringOrNull(v['code']),
    repair_id: stringOrNull(v['repair_id']),
    files: rows(v['files']).map((f) => {
      if (!has(FILE_STATE_TEXTS, f['state'])) throw invalid();
      return {
        asset_id: assetIdOf(f['asset_id']),
        path: stringOf(f['path']),
        backup_path: stringOrNull(f['backup_path']),
        state: f['state'] as RepairFileState,
      };
    }),
  };
}

// ── names and lines ───────────────────────────────────────────────────────────────────────────

/** The last component of a path. */
const lastComponent = (path: string): string =>
  path
    .split('/')
    .filter((p) => p !== '')
    .pop() ?? path;

/** A file by its path (the last component), else by its asset ID with `_` as a space. */
export function fileName(assetId: OmarchyAssetId, path?: string): string {
  return path === undefined || path === '' ? assetId.replaceAll('_', ' ') : lastComponent(path);
}

/** One file of a repair as the board shows it: `<name>: <State>` and the kept copy. */
export type FileLine = { readonly line: string; readonly kept: string };

const keptText = (backupPath: string | null): string =>
  backupPath === null ? 'No kept copy' : `Kept copy: ${backupPath}`;

/** The lines of the files of a journal (the name is the asset ID: the status has no path). */
export function journalFileLines(journal: RepairJournalView): FileLine[] {
  return journal.files.map((f) => ({
    line: `${fileName(f.asset_id)}: ${OBSERVED_TEXTS[f.state]}`,
    kept: keptText(f.backup_path),
  }));
}

/** The lines of the files of a repair result (the name is the last component of the path). */
export function resultFileLines(repair: RepairResult | undefined): FileLine[] {
  if (repair === undefined) return [];
  return repair.files.map((f) => ({
    line: `${fileName(f.asset_id, f.path)}: ${FILE_STATE_TEXTS[f.state]}`,
    kept: keptText(f.backup_path),
  }));
}

export const journalStateText = (state: JournalState): string => JOURNAL_STATES[state];
export const restoreActionText = (action: RestoreAction): string => RESTORE_ACTION_TEXTS[action];

/** The date of a journal, in the locale of the browser. */
export const journalDate = (createdAt: number): string => new Date(createdAt).toLocaleString();

/** A journal can be restored when at least one of its files has a kept copy. */
export const hasKeptCopy = (journal: RepairJournalView): boolean =>
  journal.files.some((f) => f.backup_path !== null);

/** The three actions of `omarchy_repair.v1`. */
export const isRepairAction = (action: OmarchyAction): boolean =>
  action === 'install_replace' || action === 'install_resume' || action === 'restore';

const files = (n: number): string => (n === 1 ? '1 file' : `${n} files`);

/**
 * The text of a done repair action. N counts the files that the action changed: `replaced` for a
 * replacement or a resume, `restored` for a restore.
 */
export function doneText(action: OmarchyAction, repair: RepairResult | undefined): string {
  const wanted: RepairFileState = action === 'restore' ? 'restored' : 'replaced';
  const n = repair === undefined ? 0 : repair.files.filter((f) => f.state === wanted).length;
  if (action === 'install_resume') return `Repair resumed. Replaced ${files(n)}.`;
  if (action === 'restore') return `Restored ${files(n)}. The kept copies stay.`;
  return `Replaced ${files(n)}. The kept copies are listed under Repairs.`;
}

// ── the review of a replacement ───────────────────────────────────────────────────────────────

export const REVIEW_TITLE = 'Review files to replace';
export const REVIEW_BUTTON = 'Review files to replace…';
export const CONFLICT_TEXT = 'Not recorded by Fleet Deck: replaced, and a copy is kept.';
export const RECORDED_EDIT_TEXT =
  'You edited this recorded integration file. Fleet Deck will keep it unless you select it.';
export const UNSAFE_TEXT = 'Fleet Deck cannot safely replace this path.';
export const REVIEW_CONFIRMATION =
  'Replace the listed integration files? Fleet Deck will keep a separate copy of each current file.';
export const REVIEW_CONFIRM_BUTTON = 'Keep copies and replace';
export const NO_REPLACE_TEXT = 'No file needs to be replaced.';

/** The plan can be sent: it has files, and every file can be replaced safely. */
export const canReplace = (plan: InstallPlanView): boolean =>
  plan.files.length > 0 && plan.files.every((f) => f.replaceable);

/** The recorded edits that the owner selected, in the order of the plan. */
export function selectedEdits(
  plan: InstallPlanView,
  checked: ReadonlySet<OmarchyAssetId>,
): OmarchyAssetId[] {
  return plan.files
    .filter((f) => f.classification === 'recorded_edit' && checked.has(f.asset_id))
    .map((f) => f.asset_id);
}

// ── the restore of kept copies ────────────────────────────────────────────────────────────────

export const RESTORE_TITLE = 'Restore kept copies';
export const RESTORE_BUTTON = 'Restore kept copies…';
export const RESTORE_CONFIRMATION =
  'Restore the selected files from their kept copies? A file that changed after the repair is kept aside, never overwritten.';
export const RESTORE_CONFIRM_BUTTON = 'Restore selected';
export const NOTHING_RESTORABLE_TEXT = 'Nothing can be restored.';

/** A file with one of these actions can be selected for a restore. */
export const restorable = (action: RestoreAction): boolean =>
  action === 'copy_backup' ||
  action === 'move_aside_then_copy_backup' ||
  action === 'remove_published';

/** The asset IDs of the files that a restore can change, in the order of the plan. */
export const restorableAssets = (plan: RestorePlanView): OmarchyAssetId[] =>
  plan.files.filter((f) => restorable(f.action)).map((f) => f.asset_id);

// ── the other texts ───────────────────────────────────────────────────────────────────────────

export const REPAIRS_TITLE = 'Repairs';
export const REPAIRS_ERROR_TEXT = 'The repairs could not be read.';
export const NO_REPAIRS_TEXT = 'No repairs yet.';
export const PLAN_ERROR_TEXT = 'The plan could not be read.';
export const LOADING_PLAN_TEXT = 'Loading…';
export const RESUME_BUTTON = 'Resume repair';

// ── the bodies of the three actions ───────────────────────────────────────────────────────────

/** A new request ID: 32 lowercase hex characters. */
export function newRequestId(): string {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return [...bytes].map((b) => b.toString(16).padStart(2, '0')).join('');
}

export const replaceExtra = (
  plan: InstallPlanView,
  edits: readonly OmarchyAssetId[],
  requestId: string = newRequestId(),
): ActExtra => ({
  plan_id: plan.plan_id,
  replace_recorded_edits: [...edits],
  request_id: requestId,
});

export const resumeExtra = (repairId: string, requestId: string = newRequestId()): ActExtra => ({
  repair_id: repairId,
  request_id: requestId,
});

export const restoreExtra = (
  plan: RestorePlanView,
  assets: readonly OmarchyAssetId[],
  requestId: string = newRequestId(),
): ActExtra => ({
  repair_id: plan.repair_id,
  plan_id: plan.plan_id,
  asset_ids: [...assets],
  request_id: requestId,
});

// ── the calls ─────────────────────────────────────────────────────────────────────────────────

const seg = encodeURIComponent;

/** The three reads of the repair, over the transport of the settings pages. */
export function repairCalls(io: DeckIo) {
  return {
    installPlan: async (station: string): Promise<InstallPlanView> =>
      installPlanOf(await io.get(`/api/stations/${seg(station)}/omarchy/install-plan`)),
    repairs: async (station: string): Promise<RepairStatusView> =>
      repairStatusOf(await io.get(`/api/stations/${seg(station)}/omarchy/repairs`)),
    restorePlan: async (station: string, repairId: string): Promise<RestorePlanView> =>
      restorePlanOf(
        await io.get(`/api/stations/${seg(station)}/omarchy/repairs/${seg(repairId)}/restore-plan`),
      ),
  };
}

// ── when the card offers the review ───────────────────────────────────────────────────────────

/** The code of a refused install: the card then offers the review of the files to replace. */
export const INSTALL_REFUSED = 'install_refused';

/**
 * The card shows **Review files to replace…**: nothing runs, Omarchy and its install are known, and
 * files are changed or missing or the last install was refused. The caller checks the feature.
 */
export function reviewVisible(status: OmarchyStatus, refused: boolean): boolean {
  const { omarchy, install, running } = status;
  if (running !== null || !omarchy.present || install === null) return false;
  return install.files_changed + install.files_missing > 0 || refused;
}
