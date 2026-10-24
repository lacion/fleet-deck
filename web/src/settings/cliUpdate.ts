// B28 (design `docs/v2/design/cli-update.md` §3.12): the calls and the fixed texts of the provider
// update line of the station card and of the "Update now" card of Settings. The station sends
// states and codes; the board shows only the texts of this file.
import { relativeAge } from '../app/sessions';
import type {
  CliUpdateAttempt,
  CliUpdateReport,
  CliUpdateRunAnswer,
  InstallMethod,
  ProviderKind,
  ProviderUpdateState,
} from '../gen';
import type { ElevationAsk } from './elevation';
import { DeckError, type DeckHttp, describe } from './http';

/** The body of `cli_update.run` (the elevation is added by the call). */
export type CliUpdateRunBody = {
  readonly provider: ProviderKind;
  readonly check_only: boolean;
  readonly force: boolean;
};

/** The calls of the card. `station` is the station name. */
export type CliUpdateActions = {
  /** `GET /api/stations/{station}/cli-update`. */
  status(station: string): Promise<CliUpdateReport>;
  /** `POST /api/stations/{station}/cli-update`; an update needs the elevation. */
  run(
    station: string,
    body: CliUpdateRunBody,
    elevation: string | null,
  ): Promise<CliUpdateRunAnswer>;
};

const INVALID = 'The deck sent an invalid answer.';
const bad = (): never => {
  throw new Error(INVALID);
};
const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

function stateOf(v: unknown): ProviderUpdateState {
  if (
    !record(v) ||
    typeof v['provider'] !== 'string' ||
    typeof v['method'] !== 'string' ||
    typeof v['updatable'] !== 'boolean' ||
    typeof v['channel'] !== 'string'
  ) {
    return bad();
  }
  return v as ProviderUpdateState;
}

/** The answer of `cli_update.status`, checked by shape. */
export function reportOf(v: unknown): CliUpdateReport {
  if (
    !record(v) ||
    typeof v['enabled'] !== 'boolean' ||
    typeof v['window'] !== 'string' ||
    !Array.isArray(v['providers']) ||
    !(v['running'] === null || typeof v['running'] === 'string')
  ) {
    return bad();
  }
  v['providers'].forEach(stateOf);
  return v as CliUpdateReport;
}

/** The answer of `cli_update.run`, checked by shape. */
export function answerOf(v: unknown): CliUpdateRunAnswer {
  if (
    !record(v) ||
    !(v['attempt_id'] === null || typeof v['attempt_id'] === 'string') ||
    !(v['refused'] === null || typeof v['refused'] === 'string') ||
    !Array.isArray(v['busy'])
  ) {
    return bad();
  }
  stateOf(v['state']);
  return v as CliUpdateRunAnswer;
}

const path = (station: string): string => `/api/stations/${encodeURIComponent(station)}/cli-update`;

/** The calls over the transport of the settings pages. */
export function cliUpdateActions(io: DeckHttp): CliUpdateActions {
  return {
    status: async (station) => reportOf(await io.get(path(station))),
    run: async (station, body, elevation) =>
      answerOf(await io.post(path(station), { ...body, elevation })),
  };
}

// --- the texts ---

const NAMES: Readonly<Record<string, string>> = { claude: 'Claude', codex: 'Codex' };

/** The product name of a provider. */
export const providerName = (provider: ProviderKind): string =>
  Object.hasOwn(NAMES, provider) ? (NAMES[provider] ?? 'The provider') : 'The provider';

const METHODS: Readonly<Record<string, string>> = {
  native: 'native installer',
  npm: 'npm',
  bun: 'Bun',
  homebrew: 'Homebrew',
  system_package: 'a system package',
  unknown: 'an unknown method',
};

/** The words of an install method. */
export const methodText = (method: InstallMethod): string =>
  Object.hasOwn(METHODS, method)
    ? (METHODS[method] ?? METHODS['unknown'] ?? '')
    : 'an unknown method';

const REASONS: Readonly<Record<string, string>> = {
  gpgv_missing: 'install GnuPG (gpgv) to verify Claude updates',
  tool_missing: 'the package manager was not found',
  disk_full: 'less than 1 GB free',
  channel_unreachable: 'the vendor did not answer',
  signature_invalid: 'the release signature did not verify',
  signature_unknown_key: 'the release has an unknown signing key',
  provenance_missing: 'the package has no provenance',
  known_broken: 'this version is known to break Fleet Deck',
  below_tested: 'this version is older than Fleet Deck supports',
  below_handoff: 'this version cannot run fleet tools',
  too_new: 'the new version is still in its wait time',
  publish_time_unknown: 'the release time is unknown',
  station_restarting: 'the station was restarting',
  starts_in_flight: 'sessions were starting',
  sessions_running: 'sessions ran',
  another_update: 'another update ran',
  second_install: 'a second install, update it by hand',
  not_found: 'the program was not found',
  not_a_version: 'the vendor gave no release version',
  custom_launcher: 'a custom launcher link, update it by hand',
  unsupported_layout: 'the install layout is not one the station can update',
  tool_too_old: 'the package manager is too old (npm 7 or Bun 1.2 needed)',
  unverified_package: 'the download held a package that was not signed',
  integrity_mismatch: 'the download did not match the signed release',
  checksum_mismatch: 'the download did not match the signed release',
  version_mismatch: 'the new version reported a different version',
  handshake_failed: 'Fleet Deck could not drive the new version',
  install_failed: 'the download or the install failed',
  timeout: 'the update took too long',
  process_left: 'an update process did not stop',
  recovery_pending: 'an interrupted update waits for a station restart',
  quarantined: 'starts are blocked after a failed update',
};

const SEE_STATION = 'see the station';

/** The words of a reason code; a code that the board does not know says "see the station". */
export const reasonText = (code: string | null): string =>
  code !== null && Object.hasOwn(REASONS, code) ? (REASONS[code] ?? SEE_STATION) : SEE_STATION;

export const UNSUPPORTED_TEXT =
  'This station cannot update its providers yet. Update the station first.';
export const LOADING_TEXT = 'Loading the provider updates…';
export const READ_ERROR_TEXT = 'The provider updates could not be read.';
export const ELEVATION_LABELS: readonly string[] = ['update'];
export const ELEVATION_EXPIRED_TEXT = 'Your confirmation ended. Press Update now… again.';

/** The age of a fact: `3 h ago`, or `just now` within the minute. */
export function agoOf(at: number, now: number): string {
  const age = relativeAge(at, now);
  return age === 'now' ? 'just now' : `${age} ago`;
}

// --- one line for one provider ---

/** The tone of a line: quiet text, the hazard color, or plain text for work in progress. */
export type Tone = 'quiet' | 'hazard' | 'normal';
export type LineView = { readonly text: string; readonly tone: Tone };

const handText = (name: string, method: string): string => `${name}: update by hand (${method})`;

/** The text of a quarantine (it wins over every other row). */
function quarantineText(name: string, code: string): string {
  if (code === 'recovery_pending') {
    return `${name}: an interrupted update is not finished. Restart the station.`;
  }
  if (code === 'process_left') {
    return `${name}: an update process did not stop. Restart the station.`;
  }
  return `${name}: starts blocked after a failed update. Reinstall ${name}, then press Check in Settings.`;
}

/** The row of the last attempt (the table of §3.12), without the `available` suffix. */
function attemptLine(
  name: string,
  state: ProviderUpdateState,
  last: CliUpdateAttempt,
  now: number,
): LineView | null {
  const age = agoOf(last.ended_at ?? last.started_at, now);
  const quiet = (text: string): LineView => ({ text, tone: 'quiet' });
  const hazard = (text: string): LineView => ({ text, tone: 'hazard' });
  const to = last.to_version ?? last.target_version;
  switch (last.outcome) {
    case 'updated':
      if (last.trigger === 'external') {
        return quiet(
          `${name}: changed outside Fleet Deck, ${last.from_version ?? '?'} → ${to ?? '?'} · ${age}`,
        );
      }
      return quiet(`${name}: updated ${last.from_version ?? '?'} → ${to ?? '?'} · ${age}`);
    case 'up_to_date':
      return quiet(
        `${name}: up to date · checked ${agoOf(state.checked_at ?? last.ended_at ?? last.started_at, now)}`,
      );
    case 'skipped_busy':
      return quiet(`${name}: not updated, sessions ran · ${age}`);
    case 'refused':
      return quiet(`${name}: not updated, ${reasonText(last.reason)} · ${age}`);
    case 'report_only':
      return quiet(handText(name, methodText(state.method)));
    case 'failed':
      return hazard(
        last.from_version === null
          ? `${name}: update failed · ${age}`
          : `${name}: update failed, kept ${last.from_version} · ${age}`,
      );
    case 'rolled_back': {
      const back = last.to_version ?? last.from_version;
      const backText = back === null ? 'the earlier version' : back;
      if (last.trigger === 'recovery') {
        return hazard(`${name}: an interrupted update was undone, back on ${backText} · ${age}`);
      }
      return hazard(
        `${name}: ${last.target_version ?? 'the new version'} failed the check, back on ${backText} · ${age}`,
      );
    }
    case 'verified':
      return quiet(`${name}: checked again, starts allowed · ${age}`);
    case 'rollback_failed':
      return hazard(`${name}: update and way back failed. Reinstall ${name} on this station.`);
    case 'interrupted':
      return hazard(`${name}: update interrupted · ${age}`);
    default:
      return quiet(`${name}: not updated, ${SEE_STATION} · ${age}`);
  }
}

/**
 * The line of one provider (the table of §3.12): the quarantine wins, then a running update, then
 * a method that the station only reports, then the last attempt. `null` when the station has
 * nothing to say yet.
 */
export function providerLine(
  state: ProviderUpdateState,
  running: string | null,
  now: number,
): LineView | null {
  const name = providerName(state.provider);
  if (state.quarantined !== null) {
    return { text: quarantineText(name, state.quarantined), tone: 'hazard' };
  }
  const last = state.last;
  if (running !== null && last !== null && last.attempt_id === running) {
    const target = last.target_version ?? state.available;
    return {
      text: target === null ? `${name}: updating…` : `${name}: updating to ${target}…`,
      tone: 'normal',
    };
  }
  let line: LineView | null = null;
  if (state.blocked === 'report_only_method') {
    line = { text: handText(name, methodText(state.method)), tone: 'quiet' };
  } else if (state.blocked === 'custom_launcher') {
    line = { text: handText(name, 'a custom launcher link'), tone: 'quiet' };
  } else if (last !== null) {
    line = attemptLine(name, state, last, now);
  } else if (state.blocked !== null) {
    line = { text: `${name}: not updated, ${reasonText(state.blocked)}`, tone: 'quiet' };
  }
  if (state.available !== null) {
    if (line === null) return { text: `${name}: ${state.available} available`, tone: 'quiet' };
    return { text: `${line.text} · ${state.available} available`, tone: line.tone };
  }
  return line;
}

/** The schedule line. */
export const scheduleText = (report: CliUpdateReport): string =>
  report.enabled ? `Updates: nightly ${report.window}` : 'Updates: off';

// --- the Settings card ---

/** One row title: `Claude 2.1.309 · native installer · stable channel`. */
export function rowTitle(state: ProviderUpdateState, pin: string | null): string {
  const name = providerName(state.provider);
  const head = state.installed === null ? name : `${name} ${state.installed}`;
  const channel =
    state.channel === 'pin'
      ? pin === null || pin === ''
        ? 'pinned'
        : `pinned to ${pin}`
      : `${state.channel} channel`;
  return `${head} · ${methodText(state.method)} · ${channel}`;
}

/** The result line of `Check`. */
export function checkLine(
  answer: CliUpdateRunAnswer,
  provider: ProviderKind,
  wasQuarantined: boolean,
): string {
  const name = providerName(provider);
  const state = answer.state;
  if (wasQuarantined) {
    if (answer.refused !== null) return `Still blocked: ${reasonText(answer.refused)}.`;
    if (state.quarantined !== null) return `Still blocked: ${reasonText(state.quarantined)}.`;
    return `${name} passed the checks. Sessions can start again.`;
  }
  if (answer.refused !== null) return `Not available: ${reasonText(answer.refused)}.`;
  if (state.available !== null) return `${state.available} is available.`;
  return 'Up to date.';
}

/** The target that a dialog names: the version of the last check, or "the newest version". */
export const dialogTarget = (state: ProviderUpdateState): string =>
  state.available ?? 'the newest version';

/** The title of the dialog. */
export const dialogTitle = (provider: ProviderKind, station: string): string =>
  `Update ${providerName(provider)} on ${station}?`;

/** The text of the dialog. */
export function dialogText(state: ProviderUpdateState): string {
  const target = dialogTarget(state);
  const installed = state.installed ?? 'the earlier version';
  if (state.provider === 'claude') {
    return `The station downloads Claude ${target} from Anthropic (${state.channel} channel), checks it against the signed release and checks that Fleet Deck can drive it before it switches. If a check fails after the switch, it goes back to ${installed} from a local copy. New sessions wait a few seconds while it switches.`;
  }
  const tool = state.method === 'bun' ? 'Bun' : 'npm';
  return `The station downloads Codex ${target} from the npm registry with ${tool} into a separate folder, checks every package against the registry signature and checks that Fleet Deck can drive it before it switches. If a check fails after the switch, it puts ${installed} back from a local copy. New sessions wait a few seconds while it switches.`;
}

/** What the dialog shows after a busy answer. `force` is offered for native Claude only. */
export type BusyView = { readonly text: string; readonly canForce: boolean };

export function busyView(answer: CliUpdateRunAnswer): BusyView {
  const name = providerName(answer.state.provider);
  const n = answer.busy.length;
  if (answer.state.method === 'native') {
    return {
      text: `${n} ${name} sessions run on this station. They keep their version; new sessions use the new one.`,
      canForce: true,
    };
  }
  return {
    text: `${n} ${name} sessions run on this station. An update replaces the files in place: stop these sessions first.`,
    canForce: false,
  };
}

export const runningText = (provider: ProviderKind, target: string): string =>
  `Updating ${providerName(provider)} to ${target}…`;

export const recoveryText = (provider: ProviderKind): string =>
  `An interrupted update of ${providerName(provider)} waits for the station restart. Restart the station first.`;

/** The text of a refusal that is not a busy answer. */
export function refusedText(answer: CliUpdateRunAnswer): string {
  if (answer.refused === 'recovery_pending') return recoveryText(answer.state.provider);
  return `Not updated: ${reasonText(answer.refused)}.`;
}

/** The text of a failed call. */
export function errorText(e: unknown): string {
  if (e instanceof DeckError) {
    if (e.status === 501) return UNSUPPORTED_TEXT;
    if (e.code === 'elevation_required') return ELEVATION_EXPIRED_TEXT;
  }
  return describe(e);
}

// --- the flows ---

export type CheckOutcome =
  | { readonly kind: 'done'; readonly answer: CliUpdateRunAnswer; readonly line: string }
  | { readonly kind: 'failed'; readonly text: string };

/** `Check`: no elevation; a quarantined provider shows the pass texts. */
export async function runCheck(
  actions: CliUpdateActions,
  station: string,
  state: ProviderUpdateState,
): Promise<CheckOutcome> {
  try {
    const answer = await actions.run(
      station,
      { provider: state.provider, check_only: true, force: false },
      null,
    );
    return {
      kind: 'done',
      answer,
      line: checkLine(answer, state.provider, state.quarantined !== null),
    };
  } catch (e) {
    return { kind: 'failed', text: errorText(e) };
  }
}

/** `Update now…`: the elevation first; `null` when the owner cancels. */
export const askUpdate = (ask: ElevationAsk): Promise<string | null> => ask(ELEVATION_LABELS);

export type UpdateOutcome =
  | { readonly kind: 'started'; readonly answer: CliUpdateRunAnswer }
  | { readonly kind: 'busy'; readonly answer: CliUpdateRunAnswer; readonly view: BusyView }
  | { readonly kind: 'refused'; readonly text: string }
  | { readonly kind: 'failed'; readonly text: string };

/** The update call after the dialog. `force` is sent only when it is true. */
export async function runUpdate(
  actions: CliUpdateActions,
  station: string,
  provider: ProviderKind,
  elevation: string,
  force: boolean,
): Promise<UpdateOutcome> {
  try {
    const answer = await actions.run(station, { provider, check_only: false, force }, elevation);
    if (answer.refused === 'sessions_running') {
      return { kind: 'busy', answer, view: busyView(answer) };
    }
    if (answer.refused !== null) return { kind: 'refused', text: refusedText(answer) };
    return { kind: 'started', answer };
  } catch (e) {
    return { kind: 'failed', text: errorText(e) };
  }
}

/** The wait between two reads of the poll. */
export const POLL_MS = 3_000;

/**
 * Reads the report every `POLL_MS` until `running` is `null` or `stopped()`. Every report goes to
 * `onReport`. A failed read ends the poll with `false`; an end by `running === null` gives `true`.
 */
export async function pollUntilIdle(
  actions: CliUpdateActions,
  station: string,
  io: {
    readonly sleep: (ms: number) => Promise<void>;
    readonly stopped: () => boolean;
    readonly onReport: (report: CliUpdateReport) => void;
  },
): Promise<boolean> {
  for (;;) {
    await io.sleep(POLL_MS);
    if (io.stopped()) return false;
    let report: CliUpdateReport;
    try {
      report = await actions.status(station);
    } catch {
      return false;
    }
    if (io.stopped()) return false;
    io.onReport(report);
    if (report.running === null) return true;
  }
}
