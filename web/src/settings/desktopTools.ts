// BG-1 (design `docs/v2/design/board-gaps.md` §2.3, §2.4, §2.7, §2.9): the calls and the pure texts
// of the "Desktop tools" card on the station tab of Settings. The station sends parsed facts; the
// board shows only fixed texts. The password never passes here: it is typed in the terminal.
import type {
  DesktopToolsPlan,
  KeyboardFacts,
  MaintenanceInfo,
  MaintenanceList,
  PluginStatus,
  StationTools,
  ToolFact,
} from '../gen';
import type { ShellActions } from '../shell/shell';
import type { ElevationAsk } from './elevation';
import { DeckError, type DeckHttp, describe } from './http';
import { ELEVATION_HEADER } from './omarchy';

/** The calls of the card. `station` is the station name; `id` is a maintenance ID. */
export type DesktopToolsActions = {
  /** `GET /api/stations/{station}/desktop-tools`. */
  tools(station: string): Promise<StationTools>;
  /** `GET /api/stations/{station}/desktop-tools/plan`. */
  plan(station: string): Promise<DesktopToolsPlan>;
  /** `POST /api/stations/{station}/maintenance` with the elevation header. */
  open(
    station: string,
    planId: string,
    requestId: string,
    cols: number,
    rows: number,
    elevation: string,
  ): Promise<MaintenanceInfo>;
  /** `GET /api/stations/{station}/maintenance`. */
  list(station: string): Promise<MaintenanceList>;
  /** `POST /api/stations/{station}/maintenance/{id}/resize`. */
  resize(station: string, id: string, cols: number, rows: number): Promise<void>;
  /** The WebSocket path of one maintenance run (relative to the board origin). */
  socketPath(station: string, id: string): string;
};

const INVALID = 'The deck sent an invalid answer.';
const bad = (): never => {
  throw new Error(INVALID);
};

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);
const isStr = (v: unknown): v is string => typeof v === 'string';
const isNum = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v);
const strOrNull = (v: unknown): v is string | null => v === null || isStr(v);
const isStrs = (v: unknown): v is string[] => Array.isArray(v) && v.every(isStr);
const oneOf = (v: unknown, list: readonly string[]): boolean => isStr(v) && list.includes(v);

const PROBES = ['ok', 'missing', 'unknown', 'timeout', 'failed'];
const SOURCES = ['executable', 'package', 'none'];
const FACTS = ['ok', 'unavailable', 'failed'];
const PLUGINS = ['valid', 'invalid', 'unavailable', 'unknown'];
const SWITCHES = ['on', 'off', 'unknown'];
const DESKTOPS = ['omarchy', 'hyprland', 'none'];
const STATES = ['admitted', 'running', 'exited', 'lost'];
const POSTS = ['ok', 'transaction_changed'];

function toolFactOf(v: unknown): ToolFact {
  if (
    !record(v) ||
    !isStr(v['id']) ||
    typeof v['present'] !== 'boolean' ||
    !strOrNull(v['version']) ||
    !oneOf(v['version_source'], SOURCES) ||
    !(
      v['package_matches_executable'] === null ||
      typeof v['package_matches_executable'] === 'boolean'
    ) ||
    !oneOf(v['probe'], PROBES)
  ) {
    return bad();
  }
  return v as ToolFact;
}

/** The answer of `station.tools`, checked by shape. */
export function toolsOf(v: unknown): StationTools {
  if (
    !record(v) ||
    !isNum(v['sampled_at']) ||
    !oneOf(v['desktop'], DESKTOPS) ||
    !Array.isArray(v['tools']) ||
    !record(v['keyboard']) ||
    !oneOf(v['keyboard']['status'], FACTS) ||
    !isStrs(v['keyboard']['layouts']) ||
    !record(v['plugin']) ||
    !oneOf(v['plugin']['status'], PLUGINS) ||
    !oneOf(v['passwordless_sudo'], SWITCHES)
  ) {
    return bad();
  }
  v['tools'].forEach(toolFactOf);
  return v as StationTools;
}

/** The answer of `station.desktop_tools_plan`, checked by shape. */
export function planOf(v: unknown): DesktopToolsPlan {
  if (
    !record(v) ||
    !strOrNull(v['plan_id']) ||
    !(v['expires_at'] === null || isNum(v['expires_at'])) ||
    !isStrs(v['installable']) ||
    !isStrs(v['unavailable']) ||
    !isStrs(v['already_present']) ||
    !strOrNull(v['command_display']) ||
    typeof v['system_upgrade'] !== 'boolean' ||
    !strOrNull(v['blocked']) ||
    !isStrs(v['refused']) ||
    !Array.isArray(v['transaction']) ||
    !v['transaction'].every(
      (p) => record(p) && isStr(p['repo']) && isStr(p['name']) && isStr(p['version']),
    )
  ) {
    return bad();
  }
  // A plan that can be opened has an ID and a command to show.
  if (v['blocked'] === null && v['installable'].length > 0) {
    if (v['plan_id'] === null || v['command_display'] === null) return bad();
  }
  return v as DesktopToolsPlan;
}

/** One maintenance run, checked by shape. */
export function runOf(v: unknown): MaintenanceInfo {
  if (
    !record(v) ||
    !isStr(v['maintenance_id']) ||
    !/^[0-9a-f]{32}$/.test(v['maintenance_id']) ||
    !isStr(v['request_id']) ||
    !oneOf(v['state'], STATES) ||
    !(v['exit_code'] === null || isNum(v['exit_code'])) ||
    !(v['post_check'] === null || oneOf(v['post_check'], POSTS)) ||
    !isNum(v['created_at']) ||
    !isStrs(v['tools'])
  ) {
    return bad();
  }
  return v as MaintenanceInfo;
}

/** The answer of `maintenance.list`, checked by shape. */
export function runsOf(v: unknown): MaintenanceList {
  if (!record(v) || !Array.isArray(v['runs'])) return bad();
  v['runs'].forEach(runOf);
  return v as MaintenanceList;
}

const seg = encodeURIComponent;
const base = (station: string): string => `/api/stations/${seg(station)}`;

/** The calls over the transport of the settings pages. */
export function desktopToolsActions(io: DeckHttp): DesktopToolsActions {
  return {
    tools: async (station) => toolsOf(await io.get(`${base(station)}/desktop-tools`)),
    plan: async (station) => planOf(await io.get(`${base(station)}/desktop-tools/plan`)),
    open: async (station, planId, requestId, cols, rows, elevation) =>
      runOf(
        await io.post(
          `${base(station)}/maintenance`,
          { plan_id: planId, request_id: requestId, cols, rows },
          { [ELEVATION_HEADER]: elevation },
        ),
      ),
    list: async (station) => runsOf(await io.get(`${base(station)}/maintenance`)),
    resize: async (station, id, cols, rows) => {
      await io.post(`${base(station)}/maintenance/${seg(id)}/resize`, { cols, rows });
    },
    socketPath: (station, id) => `${base(station)}/maintenance/${seg(id)}/attach`,
  };
}

/**
 * The terminal of the installation panel needs a `ShellActions`. Only the socket path and the
 * resize exist; every other member refuses, so the terminal cannot reach a shell route.
 */
export function maintenanceTerminalActions(
  actions: DesktopToolsActions,
  station: string,
): ShellActions {
  const refuse = (): Promise<never> =>
    Promise.reject(new Error('Not available in the installation terminal.'));
  return {
    list: refuse,
    open: refuse,
    close: refuse,
    handover: refuse,
    release: refuse,
    resize: (_station, id, cols, rows) => actions.resize(station, id, cols, rows),
    paste: () =>
      Promise.reject(new Error('Pasting images is not available in the installation terminal.')),
    socketPath: (_station, id) => actions.socketPath(station, id),
  };
}

/** The display names of the tool IDs of the station. */
const TOOL_NAMES: Readonly<Record<string, string>> = {
  wtype: 'wtype',
  wf_recorder: 'wf-recorder',
  wlrctl: 'wlrctl',
  grim: 'grim',
  busctl: 'busctl',
  jq: 'jq',
  hyprland: 'Hyprland',
  chromium: 'Chromium',
  notify_send: 'notify-send',
};

/** The display name of a tool ID; an ID outside the nine is shown as it is. */
export const toolName = (id: string): string =>
  Object.hasOwn(TOOL_NAMES, id) ? (TOOL_NAMES[id] ?? id) : id;

/** The tools that are applications with a product name (sans): every other tool is a command. */
const APPS: readonly string[] = ['hyprland', 'chromium'];

/** A tool is a command that the owner types (mono, §1.5), not a product name. */
export const isCommand = (id: string): boolean => !APPS.includes(id);

/**
 * The language of the common XKB layouts (xkb `base.lst`). A layout code is mostly a country code,
 * and many of them are also the code of another language (`se` Northern Sami, `ca` Catalan, `br`
 * Breton): only a layout of this list gets a name; any other shows its code alone.
 */
const LAYOUT_LANGUAGES: Readonly<Record<string, string>> = {
  us: 'en-US',
  gb: 'en-GB',
  de: 'de',
  at: 'de-AT',
  ch: 'de-CH',
  es: 'es',
  latam: 'es-419',
  fr: 'fr',
  ca: 'fr-CA',
  it: 'it',
  pt: 'pt',
  br: 'pt-BR',
  nl: 'nl',
  se: 'sv',
  no: 'nb',
  dk: 'da',
  fi: 'fi',
  is: 'is',
  ie: 'ga',
  pl: 'pl',
  cz: 'cs',
  sk: 'sk',
  hu: 'hu',
  ro: 'ro',
  si: 'sl',
  hr: 'hr',
  rs: 'sr',
  bg: 'bg',
  gr: 'el',
  tr: 'tr',
  ru: 'ru',
  ua: 'uk',
  by: 'be',
  ee: 'et',
  lv: 'lv',
  lt: 'lt',
  il: 'he',
  ara: 'ar',
  ir: 'fa',
  in: 'hi',
  th: 'th',
  vn: 'vi',
  id: 'id',
  jp: 'ja',
  kr: 'ko',
  cn: 'zh',
};

const LANGUAGES = new Intl.DisplayNames(['en'], { type: 'language', fallback: 'none' });

/** A keyboard layout in words with its code: `es` → `Spanish (es)`; another code stays as it is. */
export function layoutText(code: string): string {
  const tag = Object.hasOwn(LAYOUT_LANGUAGES, code) ? LAYOUT_LANGUAGES[code] : undefined;
  const name = tag === undefined ? undefined : LANGUAGES.of(tag);
  return name === undefined || name === '' ? code : `${name} (${code})`;
}

export const UNSUPPORTED_TEXT = "This station's build cannot install desktop tools from the board.";
export const LOADING_TEXT = 'Loading the desktop tools…';
export const READ_ERROR_TEXT = 'The desktop tools could not be read.';
export const PLAN_LOADING_TEXT = 'Reading the install plan…';
export const REFUSED_TEXT =
  'The package transaction needs packages that Fleet Deck does not allow. Nothing was installed.';
export const WLRCTL_TEXT = 'Fleet Deck has no supported package source for wlrctl on this station.';
export const CONFIRM_TEXT =
  'This opens a board terminal to install the listed packages as root. Review the transaction and enter your station password there if sudo asks.';
export const ELEVATION_LABEL = 'Install desktop tools';

/** The state of one tool: installed, not installed, or the check that did not end. */
export function probeText(fact: ToolFact): string {
  switch (fact.probe) {
    case 'ok':
      return fact.present ? 'Installed' : 'Not installed';
    case 'missing':
      return 'Not installed';
    case 'unknown':
      // The tool is there; its version is not known, so the line shows none.
      return 'Installed';
    case 'timeout':
      return 'Check timed out';
    case 'failed':
      return 'Check failed';
  }
}

/**
 * The state of a tool with its version when there is one: `Installed · 0.4`. A version read from
 * the package database, not from the program, says so: `Installed · 0.4 (package)`.
 */
export function toolState(fact: ToolFact): string {
  const state = probeText(fact);
  if (fact.version === null || state !== 'Installed') return state;
  return `${state} · ${fact.version}${fact.version_source === 'package' ? ' (package)' : ''}`;
}

export const PACKAGE_MISMATCH_TEXT = 'A different program with this name is used on the desktop.';

/** The state of the keyboard layouts: the layouts, or why there are none. */
export function keyboardState(keyboard: KeyboardFacts): string {
  switch (keyboard.status) {
    case 'ok':
      return keyboard.layouts.length === 0 ? 'None' : keyboard.layouts.map(layoutText).join(', ');
    case 'unavailable':
      return 'Not available';
    case 'failed':
      return 'Check failed';
  }
}

const PLUGIN_STATES: Readonly<Record<PluginStatus, string>> = {
  valid: 'Installed',
  invalid: 'Not valid',
  unavailable: 'Not installed',
  unknown: 'Check failed',
};

/** The state of the Fleet plugin of Hyprland. */
export const pluginState = (status: PluginStatus): string => PLUGIN_STATES[status];

/** The status glyph of a row (`.fd-dot`): there, absent (or not checked), or broken. */
export type RowStatus = 'online' | 'idle' | 'lost';

/** One row of the desktop tools list. `code`: the name is a program name (mono). */
export type ToolRow = {
  readonly key: string;
  readonly name: string;
  readonly code: boolean;
  readonly state: string;
  readonly status: RowStatus;
};

/** The rows of the facts: the tools in the answer order, then the keyboard and the plugin. */
export function toolRows(tools: StationTools): ToolRow[] {
  const rows: ToolRow[] = tools.tools.map((fact) => {
    const state = toolState(fact);
    return {
      key: fact.id,
      name: toolName(fact.id),
      code: isCommand(fact.id),
      state,
      status: state.startsWith('Installed') ? 'online' : 'idle',
    };
  });
  rows.push({
    key: 'keyboard',
    name: 'Keyboard layouts',
    code: false,
    state: keyboardState(tools.keyboard),
    status: tools.keyboard.status === 'ok' ? 'online' : 'idle',
  });
  const plugin = tools.plugin.status;
  rows.push({
    key: 'plugin',
    name: 'Fleet plugin',
    code: false,
    state: pluginState(plugin),
    status: plugin === 'valid' ? 'online' : plugin === 'invalid' ? 'lost' : 'idle',
  });
  return rows;
}

/** The line above the list: `2 of 9 tools installed.` */
export function toolsSummary(tools: StationTools): string {
  const there = tools.tools.filter((f) => probeText(f) === 'Installed').length;
  const all = tools.tools.length;
  return all === 1 ? `${there} of 1 tool installed.` : `${there} of ${all} tools installed.`;
}

export const SUDO_TEXT = 'Passwordless sudo is enabled on this station.';

/** What the install panel shows for a plan. `open` is true only for the normal plan. */
export type PlanView = {
  readonly open: boolean;
  /** The text lines above the command, in order. */
  readonly lines: readonly string[];
  /** The `command_display` and the transaction lines, for the normal plan. */
  readonly command: string | null;
  readonly transaction: readonly string[];
};

const names = (ids: readonly string[]): string => ids.map(toolName).join(', ');

/** The states of a plan (decision 3): blocked, nothing to install, or the normal plan. */
export function planView(plan: DesktopToolsPlan): PlanView {
  const wlrctl = plan.unavailable.includes('wlrctl') ? [WLRCTL_TEXT] : [];
  if (plan.blocked !== null) {
    const lines = [REFUSED_TEXT];
    if (plan.refused.length > 0) lines.push(`Not allowed: ${plan.refused.join(', ')}`);
    return { open: false, lines, command: null, transaction: [] };
  }
  if (plan.installable.length === 0) {
    return {
      open: false,
      lines: ['All installable desktop tools are present.', ...wlrctl],
      command: null,
      transaction: [],
    };
  }
  const lines = [`To install: ${names(plan.installable)}`];
  if (plan.already_present.length > 0)
    lines.push(`Already present: ${names(plan.already_present)}`);
  lines.push(...wlrctl);
  return {
    open: true,
    lines,
    command: plan.command_display,
    transaction: plan.transaction.map((p) => `${p.repo}/${p.name} ${p.version}`),
  };
}

const OPEN_ERRORS: Readonly<Record<string, string>> = {
  shell_disabled:
    'Board terminals are disabled on this station. Enable them in station settings to install desktop tools.',
  plan_changed: 'The plan changed or expired. Review the desktop tools again.',
  maintenance_busy: 'Another installation is running on this station.',
  transaction_refused: REFUSED_TEXT,
  request_id_conflict: 'This request was already used for another plan.',
};

/** The text of a failed plan read: the unsupported text for a 501, else the deck's message. */
export function planErrorText(e: unknown): string {
  return e instanceof DeckError && e.status === 501 ? UNSUPPORTED_TEXT : describe(e);
}

/** The text of a failed open: a fixed text for a known code, else the deck's message. */
export function openErrorText(e: unknown): string {
  if (e instanceof DeckError) {
    if (e.status === 501) return UNSUPPORTED_TEXT;
    if (e.code !== null && Object.hasOwn(OPEN_ERRORS, e.code)) {
      return OPEN_ERRORS[e.code] ?? describe(e);
    }
  }
  return describe(e);
}

/** 32 lowercase hex characters from 128 random bits. */
export function newRequestId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
}

/** One request ID for each plan ID: a second press of the same plan sends the same ID. */
export function makeRequestIds(make: () => string = newRequestId): (planId: string) => string {
  let held: { readonly planId: string; readonly requestId: string } | null = null;
  return (planId) => {
    if (held === null || held.planId !== planId) held = { planId, requestId: make() };
    return held.requestId;
  };
}

/** The terminal size that the board asks for. */
export const OPEN_COLS = 100;
export const OPEN_ROWS = 30;

export type OpenOutcome =
  | { readonly kind: 'cancelled' }
  | { readonly kind: 'opened'; readonly run: MaintenanceInfo }
  | { readonly kind: 'failed'; readonly text: string };

/** Asks the elevation, then opens the run. `null` from the elevation sends nothing. */
export async function openInstall(
  actions: DesktopToolsActions,
  ask: ElevationAsk,
  station: string,
  planId: string,
  requestId: string,
): Promise<OpenOutcome> {
  const token = await ask([ELEVATION_LABEL]);
  if (token === null) return { kind: 'cancelled' };
  try {
    const run = await actions.open(station, planId, requestId, OPEN_COLS, OPEN_ROWS, token);
    return { kind: 'opened', run };
  } catch (e) {
    return { kind: 'failed', text: openErrorText(e) };
  }
}

export const CONNECTING_TEXT = 'Connecting…';
export const GONE_TEXT = 'This installation is no longer listed.';
export const CHANGED_TEXT = 'The installed packages differ from the reviewed plan.';
export const LOST_TEXT =
  'The installation runner ended without a result. Check the desktop tools again.';

/** The state lines of a run (the first line is the state; a second line may follow). */
export function stateLines(run: MaintenanceInfo): string[] {
  switch (run.state) {
    case 'admitted':
      return ['Starting…'];
    case 'running':
      return ['Installing… Enter your station password in the terminal if sudo asks.'];
    case 'lost':
      return [LOST_TEXT];
    case 'exited': {
      let main: string;
      if (run.exit_code === null) main = 'The package transaction ended without an exit code.';
      else if (run.exit_code !== 0) {
        main = `The package transaction ended with code ${run.exit_code}.`;
      } else if (run.post_check === null) {
        main = 'The package transaction finished. The result could not be checked.';
      } else main = 'The package transaction finished.';
      return run.post_check === 'transaction_changed' ? [main, CHANGED_TEXT] : [main];
    }
  }
}

/** The short state text of a row of the Installations list. */
export function shortState(run: MaintenanceInfo): string {
  switch (run.state) {
    case 'admitted':
      return 'Starting';
    case 'running':
      return 'Running';
    case 'lost':
      return 'Runner lost';
    case 'exited':
      if (run.exit_code === null) return 'Ended';
      return run.exit_code === 0 ? 'Finished' : `Ended with code ${run.exit_code}`;
  }
}

/** After the exit: a line for each tool of the run, from the fresh tool facts. */
export function postExitLines(runTools: readonly string[], tools: StationTools): string[] {
  const out: string[] = [];
  for (const id of runTools) {
    const fact = tools.tools.find((t) => t.id === id);
    if (fact === undefined) continue;
    const name = toolName(id);
    out.push(
      fact.present
        ? `${name}: Installed. Desktop compatibility has not been verified.`
        : `The package transaction finished, but ${name} is still missing from the desktop PATH.`,
    );
  }
  return out;
}
