// Omarchy admin task D (design `docs/v2/design/omarchy-admin.md` §5): the calls and the pure texts
// of the Omarchy card on the station tab of Settings. The station sends the status; the board
// shows only fixed texts and the message of a failed action as plain text.
import type {
  ActOutcome,
  OmarchyAction,
  OmarchyActResult,
  OmarchyInstall,
  OmarchyScreensaver,
  OmarchyStatus,
  ProvisionView,
} from '../gen';
import type { ElevationAsk } from './elevation';
import { DeckError, type DeckIo } from './http';
import {
  type ActExtra,
  type InstallPlanView,
  type RepairResult,
  type RepairStatusView,
  type RestorePlanView,
  repairCalls,
  repairResultOf,
} from './omarchyRepair';

export type {
  ActOutcome,
  OmarchyAction,
  OmarchyActResult,
  OmarchyInstall,
  OmarchyScreensaver,
  OmarchyStatus,
  ProvisionView,
};

/** The calls of the card. `station` is the station name. */
export type OmarchyActions = {
  /** `GET /api/stations/{station}/omarchy`. */
  status(station: string): Promise<OmarchyStatus>;
  /** `POST /api/stations/{station}/omarchy` with the elevation header. */
  act(
    station: string,
    action: OmarchyAction,
    elevation: string,
    extra?: ActExtra,
  ): Promise<OmarchyActResult>;
  /** `GET /api/stations/{station}/omarchy/install-plan`. */
  installPlan(station: string): Promise<InstallPlanView>;
  /** `GET /api/stations/{station}/omarchy/repairs`. */
  repairs(station: string): Promise<RepairStatusView>;
  /** `GET /api/stations/{station}/omarchy/repairs/{repairId}/restore-plan`. */
  restorePlan(station: string, repairId: string): Promise<RestorePlanView>;
};

/** The header that carries the elevation (design §4). */
export const ELEVATION_HEADER = 'x-fleetdeck-elevation';

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

const orNull = (v: unknown): unknown => (v === undefined ? null : v);

/** A status as the station sent it; a missing optional part reads as `null`. */
function statusOf(v: unknown): OmarchyStatus {
  if (!record(v) || !record(v['omarchy']) || typeof v['omarchy']['present'] !== 'boolean') {
    throw new Error('The deck sent an invalid Omarchy status.');
  }
  return {
    omarchy: v['omarchy'],
    install: orNull(v['install']),
    screensaver: orNull(v['screensaver']),
    provision: orNull(v['provision']),
    running: orNull(v['running']),
  } as OmarchyStatus;
}

const OUTCOMES: readonly string[] = ['done', 'failed', 'started'];

function resultOf(v: unknown): OmarchyActResult {
  if (!record(v) || typeof v['outcome'] !== 'string' || !OUTCOMES.includes(v['outcome'])) {
    throw new Error('The deck sent an invalid Omarchy answer.');
  }
  const out = {
    outcome: v['outcome'],
    code: typeof v['code'] === 'string' ? v['code'] : null,
    message: typeof v['message'] === 'string' ? v['message'] : null,
    status: statusOf(v['status']),
  } as OmarchyActResult;
  if (v['repair'] !== undefined && v['repair'] !== null) out.repair = repairResultOf(v['repair']);
  return out;
}

const seg = encodeURIComponent;

/** The Omarchy calls over the transport of the settings pages. */
export function omarchyActions(io: DeckIo): OmarchyActions {
  return {
    status: async (station) => statusOf(await io.get(`/api/stations/${seg(station)}/omarchy`)),
    act: async (station, action, elevation, extra) =>
      resultOf(
        await io.post(
          `/api/stations/${seg(station)}/omarchy`,
          { action, ...extra },
          { [ELEVATION_HEADER]: elevation },
        ),
      ),
    ...repairCalls(io),
  };
}

/** The label of the button of an action. */
export function actionLabel(action: OmarchyAction): string {
  switch (action) {
    case 'install':
      return 'Install';
    case 'uninstall':
      // It asks for the password and removes the integration: the ellipsis says that more comes.
      return 'Uninstall…';
    case 'screensaver_on':
      return 'Turn the screensaver on';
    case 'screensaver_off':
      return 'Turn the screensaver off';
    case 'screensaver_rebaseline':
      return 'Use the current screensaver as the original';
    case 'provision_retry':
      return 'Retry provisioning';
    case 'install_replace':
      return 'Replace changed files';
    case 'install_resume':
      return 'Resume the repair';
    case 'restore':
      return 'Restore kept copies';
  }
}

export const OUTDATED_TEXT = 'Update the integration first: press Install, then wait two minutes.';
export const UNSUPPORTED_TEXT = "This station's build cannot manage Omarchy from the board.";
export const LOADING_TEXT = 'Loading the Omarchy state…';
export const READ_ERROR_TEXT = 'The Omarchy state could not be read.';
export const WORKING_TEXT = 'Working…';

const ERROR_TEXTS: Readonly<Record<string, string>> = {
  install_refused: 'The installer refused: some files were changed.',
  uninstall_incomplete: 'The uninstall did not finish.',
  screensaver_refused: 'The screensaver change was refused.',
  integration_outdated: OUTDATED_TEXT,
  timeout: 'The station did not finish in time.',
  output_too_large: 'The command printed too much output.',
  failed: 'The command failed.',
  elevation_required: 'This action needs your confirmation.',
  busy: 'Another Omarchy action is running on this station.',
  plan_changed: 'The files changed since this review. Review the replacement plan again.',
  unsafe_path: 'Fleet Deck cannot safely replace one of these paths.',
  backup_failed: 'Fleet Deck could not keep a copy of a file. The repair stopped.',
  repair_incomplete:
    'The repair stopped after changing some files. Review the kept copies and repair status.',
  restore_conflict:
    'A current file changed after the repair. Fleet Deck kept both versions and did not overwrite it.',
  build_mismatch: 'This repair was started by another build. Restore the kept copies instead.',
  omarchy_missing: 'Omarchy was not found on this station.',
  not_provisioned: 'Provisioning cannot be retried from the board in this state.',
  unsupported: UNSUPPORTED_TEXT,
};

/** The fixed text of an error code (a `failed` answer or an HTTP error). */
export function errorText(code: string | null): string {
  const text = code !== null && Object.hasOwn(ERROR_TEXTS, code) ? ERROR_TEXTS[code] : undefined;
  return text ?? 'The action failed.';
}

/** The owner text of a terminal code: the "Cause" cell of `operations.md` §15.3. */
const TERMINAL_TEXTS: Readonly<Record<string, string>> = {
  paired_elsewhere: 'The station is already paired with a deck that has another fingerprint.',
  station_identity_changed: 'The station key is not the key that the bootstrap saved first.',
  config_changed: 'The provisioning file changed after the pairing started.',
  release_untrusted:
    'The trusted manifest or an installed hook file is missing, is not owned by root, or does not match its hash.',
  build_mismatch:
    'The running build or the binary of the station does not match the trusted manifest.',
  service_conflict:
    'The station service that is installed has another binary or another data folder.',
  provider_probe_failed:
    'The station reports a configured provider as `broken` or `unknown`, or without a version.',
  integration_failed: 'The Omarchy integration install or its status check did not succeed.',
};

const stoppedText = (code: string): string =>
  Object.hasOwn(TERMINAL_TEXTS, code)
    ? `Provisioning stopped: ${TERMINAL_TEXTS[code]}`
    : `Provisioning stopped: ${code}.`;

/** The glyph of a fact (`.fd-dot`): there or on, off or absent, broken, or running now. */
export type FactStatus = 'online' | 'idle' | 'unreachable' | 'working';

/** One fact of the card: a glyph, a name and a state in words (the Desktop tools grid). */
export type CardFact = {
  readonly name: string;
  readonly state: string;
  readonly status: FactStatus;
};

/** What the card shows of a status: the version for its heading, the facts, then the notes. */
export type CardView = {
  /** `Omarchy 4.0.0`, after the heading; `null` when the station does not know the version. */
  readonly version: string | null;
  readonly facts: readonly CardFact[];
  /** The sentences under the facts: what is wrong and what to do. */
  readonly notes: readonly string[];
};

/** The steps of provisioning in words. */
const PHASE_TEXTS: Readonly<Record<string, string>> = {
  preparing: 'Preparing',
  paired: 'Paired',
  link: 'Linking',
  integration: 'Installing the integration',
};

/** The state of a fact: its words and its glyph. */
type FactState = Omit<CardFact, 'name'>;

const ON: FactState = { state: 'On', status: 'online' };
const OFF: FactState = { state: 'Off', status: 'idle' };
const BLOCKED: FactState = { state: 'On, blocked', status: 'unreachable' };
const ADDED: FactState = { state: 'Added', status: 'online' };
const MISSING: FactState = { state: 'Missing', status: 'idle' };

/** The last retry of provisioning: its words and its glyph. */
const RETRY_FACTS: Readonly<Record<string, FactState>> = {
  running: { state: 'Running', status: 'working' },
  complete: { state: 'Complete', status: 'online' },
  terminal: { state: 'Stopped with an error', status: 'unreachable' },
  timeout: { state: 'Timed out', status: 'unreachable' },
  failed: { state: 'Failed', status: 'unreachable' },
};

const filesBad = (i: OmarchyInstall): number => i.files_changed + i.files_missing;

function installFacts(i: OmarchyInstall): CardFact[] {
  const n = filesBad(i);
  let integration: FactState = { state: 'Not installed', status: 'idle' };
  if (i.installed) {
    integration =
      n === 0
        ? { state: 'Installed', status: 'online' }
        : { state: `Installed, ${n} files changed or missing`, status: 'unreachable' };
  }
  return [
    { name: 'Integration', ...integration },
    { name: 'Fleet plugin', ...(i.plugin_enabled ? ON : OFF) },
    { name: 'Hyprland config line', ...(i.hyprland_line ? ADDED : MISSING) },
  ];
}

function provisionFact(p: ProvisionView): CardFact {
  const phase = p.phase === null ? null : (PHASE_TEXTS[p.phase] ?? p.phase);
  if (p.error !== null) return { name: 'Provisioning', state: 'Stopped', status: 'unreachable' };
  if (p.bootstrap_running) {
    return { name: 'Provisioning', state: `${phase ?? 'Running'}…`, status: 'working' };
  }
  if (p.phase === 'complete') return { name: 'Provisioning', state: 'Complete', status: 'online' };
  return {
    name: 'Provisioning',
    state: phase === null ? 'Did not finish' : `${phase}, did not finish`,
    status: 'idle',
  };
}

/** The card for a status, in order (design §5; the brief's item 3). Never "unknown" (§1.7). */
export function cardView(status: OmarchyStatus): CardView {
  const { omarchy, install, screensaver, provision, running } = status;
  const facts: CardFact[] = [];
  const notes: string[] = [];
  if (!omarchy.present) notes.push('Omarchy was not found on this station.');
  if (omarchy.present && install !== null) {
    facts.push(...installFacts(install));
    if (install.outdated) notes.push(OUTDATED_TEXT);
  }
  if (screensaver !== null) {
    const blocked = screensaver.enabled && screensaver.blocked !== null;
    facts.push({ name: 'Screensaver', ...(blocked ? BLOCKED : screensaver.enabled ? ON : OFF) });
    if (blocked) {
      notes.push('The original screensaver that Fleet Deck kept differs from the current file.');
    } else if (!screensaver.enabled && screensaver.file_restored === false) {
      notes.push(
        'The screensaver file changed after Fleet Deck restored it; the original is kept as a backup.',
      );
    }
  }
  if (provision !== null) {
    facts.push(provisionFact(provision));
    const retry = provision.last_retry === null ? undefined : RETRY_FACTS[provision.last_retry];
    if (retry !== undefined) facts.push({ name: 'Last retry', ...retry });
    if (provision.error !== null) notes.push(stoppedText(provision.error));
  }
  if (running !== null) notes.push(WORKING_TEXT);
  return {
    version: omarchy.present && omarchy.version !== null ? `Omarchy ${omarchy.version}` : null,
    facts,
    notes,
  };
}

export type CardButton = { readonly action: OmarchyAction; readonly label: string };

const RETRY_PHASES: readonly string[] = ['paired', 'link', 'integration'];

/** The integration actions: the safe ones first, the one that removes (Uninstall…) last. */
function integrationActions(install: OmarchyInstall, screensaver: OmarchyScreensaver | null) {
  const out: OmarchyAction[] = [];
  const ready = install.installed && !install.outdated;
  if (!install.installed || filesBad(install) > 0 || install.outdated) out.push('install');
  if (ready && screensaver !== null) {
    if (!screensaver.enabled) out.push('screensaver_on');
    if (screensaver.enabled) out.push('screensaver_off');
    if (screensaver.blocked !== null) out.push('screensaver_rebaseline');
  }
  return out;
}

const retryAllowed = (p: ProvisionView | null): boolean =>
  p !== null &&
  p.phase !== null &&
  RETRY_PHASES.includes(p.phase) &&
  (p.error !== null || !p.bootstrap_running);

/** The buttons that the card shows, in order; Uninstall… is the last. None while an action runs. */
export function buttons(status: OmarchyStatus): CardButton[] {
  const { omarchy, install, screensaver, provision, running } = status;
  if (running !== null) return [];
  const present = omarchy.present && install !== null;
  const out: OmarchyAction[] = present ? integrationActions(install, screensaver) : [];
  if (retryAllowed(provision)) out.push('provision_retry');
  if (present && install.installed && !install.outdated) out.push('uninstall');
  return out.map((action) => ({ action, label: actionLabel(action) }));
}

/** What the card shows before it has a status. */
export type CardState =
  | { readonly kind: 'loading' }
  | { readonly kind: 'unsupported' }
  | { readonly kind: 'hidden' }
  | { readonly kind: 'error' }
  | { readonly kind: 'ready'; readonly status: OmarchyStatus };

/** The state for a failed read: 501 and 404 have their own states. */
export function stateOfReadError(e: unknown): CardState {
  if (e instanceof DeckError && e.status === 501) return { kind: 'unsupported' };
  if (e instanceof DeckError && e.status === 404) return { kind: 'hidden' };
  return { kind: 'error' };
}

/** How an action ended, for the card. */
export type ActEnd =
  | { readonly kind: 'cancelled' }
  | {
      readonly kind: 'done';
      readonly status: OmarchyStatus;
      readonly message: string | null;
      /** The result of a repair action. */
      readonly repair?: RepairResult;
    }
  | { readonly kind: 'started'; readonly status: OmarchyStatus }
  | {
      readonly kind: 'failed';
      readonly text: string;
      readonly message: string | null;
      /** The status of the answer; `null` for an HTTP error. */
      readonly status: OmarchyStatus | null;
      /** The code of a `failed` answer (not of an HTTP error). */
      readonly code?: string;
      /** The result of a repair action. */
      readonly repair?: RepairResult;
    };

/** The label that every action gives to the elevation dialog. */
export const ELEVATION_LABELS: readonly string[] = ['Omarchy integration'];

/** Asks the elevation (a `null` sends nothing), then sends the action. */
export async function runAction(
  actions: OmarchyActions,
  station: string,
  action: OmarchyAction,
  ask: ElevationAsk,
  extra?: ActExtra,
): Promise<ActEnd> {
  const token = await ask(ELEVATION_LABELS);
  if (token === null) return { kind: 'cancelled' };
  let result: OmarchyActResult;
  try {
    result = await actions.act(station, action, token, extra);
  } catch (e) {
    return {
      kind: 'failed',
      text: errorText(e instanceof DeckError ? e.code : null),
      message: null,
      status: null,
    };
  }
  if (result.outcome === 'failed') {
    return {
      kind: 'failed',
      text: errorText(result.code ?? 'failed'),
      message: result.message,
      status: result.status,
      code: result.code ?? 'failed',
      ...(result.repair === undefined ? {} : { repair: result.repair }),
    };
  }
  if (result.outcome === 'done') {
    return {
      kind: 'done',
      status: result.status,
      message: result.message,
      ...(result.repair === undefined ? {} : { repair: result.repair }),
    };
  }
  return { kind: 'started', status: result.status };
}

/** The poll after `started`: every 5 s, for at most 10 min. */
export const POLL_MS = 5_000;
export const POLL_LIMIT_MS = 600_000;

/** The retry is over: nothing runs, and the last retry is not `running`. */
export const retryOver = (status: OmarchyStatus): boolean =>
  status.provision === null ||
  (!status.provision.bootstrap_running && status.provision.last_retry !== 'running');

export type PollEnv = {
  readonly sleep: (ms: number) => Promise<void>;
  readonly now: () => number;
  /** True when the card is gone: the poll ends. */
  readonly stopped: () => boolean;
  readonly onStatus: (status: OmarchyStatus) => void;
};

/** Reads the status until the retry is over, the limit passes, or `stopped()`. */
export async function pollRetry(
  actions: OmarchyActions,
  station: string,
  env: PollEnv,
): Promise<void> {
  const start = env.now();
  while (env.now() - start < POLL_LIMIT_MS) {
    await env.sleep(POLL_MS);
    if (env.stopped()) return;
    try {
      const status = await actions.status(station);
      if (env.stopped()) return;
      env.onStatus(status);
      if (retryOver(status)) return;
    } catch {
      // A failed read does not end the poll: the next read may work.
    }
  }
}
