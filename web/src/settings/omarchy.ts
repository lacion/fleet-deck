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
  act(station: string, action: OmarchyAction, elevation: string): Promise<OmarchyActResult>;
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
  return {
    outcome: v['outcome'],
    code: typeof v['code'] === 'string' ? v['code'] : null,
    message: typeof v['message'] === 'string' ? v['message'] : null,
    status: statusOf(v['status']),
  } as OmarchyActResult;
}

const seg = encodeURIComponent;

/** The Omarchy calls over the transport of the settings pages. */
export function omarchyActions(io: DeckIo): OmarchyActions {
  return {
    status: async (station) => statusOf(await io.get(`/api/stations/${seg(station)}/omarchy`)),
    act: async (station, action, elevation) =>
      resultOf(
        await io.post(
          `/api/stations/${seg(station)}/omarchy`,
          { action },
          { [ELEVATION_HEADER]: elevation },
        ),
      ),
  };
}

/** The label of the button of an action. */
export function actionLabel(action: OmarchyAction): string {
  switch (action) {
    case 'install':
      return 'Install';
    case 'uninstall':
      return 'Uninstall';
    case 'screensaver_on':
      return 'Turn the screensaver on';
    case 'screensaver_off':
      return 'Turn the screensaver off';
    case 'screensaver_rebaseline':
      return 'Use the current screensaver as the original';
    case 'provision_retry':
      return 'Retry provisioning';
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

const RETRY_TEXTS: Readonly<Record<string, string>> = {
  running: 'running',
  complete: 'complete',
  terminal: 'stopped with an error',
  timeout: 'timed out',
  failed: 'failed',
};

const filesBad = (i: OmarchyInstall): number => i.files_changed + i.files_missing;

function integrationLine(i: OmarchyInstall): string {
  if (!i.installed) return 'Integration: not installed';
  const n = filesBad(i);
  return n === 0
    ? 'Integration: installed'
    : `Integration: installed, ${n} files changed or missing`;
}

function installLines(i: OmarchyInstall): string[] {
  const out = [
    integrationLine(i),
    `Plugin: ${i.plugin_enabled ? 'on' : 'off'}`,
    `Hyprland line: ${i.hyprland_line ? 'present' : 'missing'}`,
  ];
  if (i.outdated) out.push(OUTDATED_TEXT);
  return out;
}

function screensaverLine(s: OmarchyScreensaver): string {
  if (s.enabled) {
    return s.blocked === null
      ? 'Screensaver: on'
      : 'Screensaver: on, but blocked: the original screensaver that Fleet Deck kept differs from the current file.';
  }
  return s.file_restored === false
    ? 'Screensaver: off. The screensaver file changed after Fleet Deck restored it; the original is kept as a backup.'
    : 'Screensaver: off';
}

function provisionLines(p: ProvisionView): string[] {
  const out = [`Provisioning: ${p.phase ?? 'unknown'}`];
  if (p.error !== null) out.push(stoppedText(p.error));
  if (p.bootstrap_running) out.push('Provisioning is running');
  else if (p.error === null && p.phase !== 'complete') out.push('Provisioning did not finish');
  if (p.last_retry !== null) out.push(`Last retry: ${RETRY_TEXTS[p.last_retry] ?? 'unknown'}`);
  return out;
}

/** The lines of the card, in order (design §5; the brief's item 3). */
export function cardLines(status: OmarchyStatus): string[] {
  const { omarchy, install, screensaver, provision, running } = status;
  const out: string[] = [];
  if (!omarchy.present) out.push('Omarchy was not found on this station.');
  else
    out.push(omarchy.version === null ? 'Omarchy (version unknown)' : `Omarchy ${omarchy.version}`);
  if (omarchy.present && install !== null) out.push(...installLines(install));
  if (screensaver !== null) out.push(screensaverLine(screensaver));
  if (provision !== null) out.push(...provisionLines(provision));
  if (running !== null) out.push(WORKING_TEXT);
  return out;
}

export type CardButton = { readonly action: OmarchyAction; readonly label: string };

const RETRY_PHASES: readonly string[] = ['paired', 'link', 'integration'];

function integrationActions(install: OmarchyInstall, screensaver: OmarchyScreensaver | null) {
  const out: OmarchyAction[] = [];
  const ready = install.installed && !install.outdated;
  if (!install.installed || filesBad(install) > 0 || install.outdated) out.push('install');
  if (ready) out.push('uninstall');
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

/** The buttons that the card shows, in order. None while an action runs. */
export function buttons(status: OmarchyStatus): CardButton[] {
  const { omarchy, install, screensaver, provision, running } = status;
  if (running !== null) return [];
  const out: OmarchyAction[] =
    omarchy.present && install !== null ? integrationActions(install, screensaver) : [];
  if (retryAllowed(provision)) out.push('provision_retry');
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
  | { readonly kind: 'done'; readonly status: OmarchyStatus; readonly message: string | null }
  | { readonly kind: 'started'; readonly status: OmarchyStatus }
  | {
      readonly kind: 'failed';
      readonly text: string;
      readonly message: string | null;
      /** The status of the answer; `null` for an HTTP error. */
      readonly status: OmarchyStatus | null;
    };

/** The label that every action gives to the elevation dialog. */
export const ELEVATION_LABELS: readonly string[] = ['Omarchy integration'];

/** Asks the elevation (a `null` sends nothing), then sends the action. */
export async function runAction(
  actions: OmarchyActions,
  station: string,
  action: OmarchyAction,
  ask: ElevationAsk,
): Promise<ActEnd> {
  const token = await ask(ELEVATION_LABELS);
  if (token === null) return { kind: 'cancelled' };
  let result: OmarchyActResult;
  try {
    result = await actions.act(station, action, token);
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
    };
  }
  if (result.outcome === 'done') {
    return { kind: 'done', status: result.status, message: result.message };
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
