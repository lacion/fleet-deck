// R5 (DL-168): the automations page's calls and rules. The deck owns the schedules and runs them;
// the board lists, edits and starts them (owner login, admin scope). The board client sends only
// GET and POST, so an update and a delete use the POST routes of the deck.
import type { Schedule, ScheduleInput, ScheduleRun, ScheduleRunState, ScheduleView } from '../gen';

type Get = (path: string) => Promise<unknown>;
type Post = (path: string, body: unknown) => Promise<unknown>;

export type AutomationActions = {
  list(): Promise<ScheduleView[]>;
  create(input: ScheduleInput): Promise<Schedule>;
  update(id: string, input: ScheduleInput): Promise<Schedule>;
  remove(id: string): Promise<void>;
  /** "Run now": the deck answers with the claimed run; the work goes on at the deck. */
  runNow(id: string): Promise<ScheduleRun>;
  runs(id: string): Promise<ScheduleRun[]>;
};

export const RUN_STATES: readonly ScheduleRunState[] = ['started', 'skipped', 'failed', 'missed'];

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

/** Validates one schedule from the deck. */
export function scheduleView(v: unknown): Schedule {
  if (
    !record(v) ||
    typeof v['schedule_id'] !== 'string' ||
    typeof v['name'] !== 'string' ||
    typeof v['enabled'] !== 'boolean' ||
    typeof v['cron'] !== 'string' ||
    typeof v['timezone'] !== 'string' ||
    typeof v['station_id'] !== 'string' ||
    typeof v['path'] !== 'string' ||
    typeof v['provider_instance'] !== 'string' ||
    typeof v['prompt'] !== 'string' ||
    typeof v['reuse_session'] !== 'boolean' ||
    typeof v['grace_ms'] !== 'number'
  ) {
    throw new Error('The deck sent an invalid automation.');
  }
  return v as Schedule;
}

/** Validates one run from the deck. */
export function runView(v: unknown): ScheduleRun {
  if (
    !record(v) ||
    typeof v['run_id'] !== 'string' ||
    typeof v['schedule_id'] !== 'string' ||
    typeof v['due_at'] !== 'number' ||
    typeof v['started_at'] !== 'number' ||
    typeof v['state'] !== 'string' ||
    !RUN_STATES.includes(v['state'] as ScheduleRunState) ||
    typeof v['reason'] !== 'string'
  ) {
    throw new Error('The deck sent an invalid run.');
  }
  return v as ScheduleRun;
}

function listView(v: unknown): ScheduleView {
  if (!record(v) || !(v['next_run_at'] === null || typeof v['next_run_at'] === 'number')) {
    throw new Error('The deck sent an invalid automation list.');
  }
  return {
    schedule: scheduleView(v['schedule']),
    next_run_at: v['next_run_at'],
    last_run: v['last_run'] === null || v['last_run'] === undefined ? null : runView(v['last_run']),
  };
}

const array = (v: unknown): unknown[] => {
  if (!Array.isArray(v)) throw new Error('The deck sent an invalid list.');
  return v;
};

/** The automation calls (`/api/schedules`). */
export function automationActions(get: Get, post: Post): AutomationActions {
  return {
    list: async () => array(await get('/api/schedules')).map(listView),
    create: async (input) => scheduleView(await post('/api/schedules', input)),
    update: async (id, input) =>
      scheduleView(await post(`/api/schedules/${encodeURIComponent(id)}/update`, input)),
    remove: async (id) => {
      await post(`/api/schedules/${encodeURIComponent(id)}/delete`, {});
    },
    runNow: async (id) => runView(await post(`/api/schedules/${encodeURIComponent(id)}/run`, {})),
    runs: async (id) =>
      array(await get(`/api/schedules/${encodeURIComponent(id)}/runs`)).map(runView),
  };
}

/** The presets of the deck's cron parser, with their meaning. */
export const PRESETS: readonly { readonly value: string; readonly label: string }[] = [
  { value: '@hourly', label: 'Every hour' },
  { value: '@daily', label: 'Every day at 00:00' },
  { value: '@weekdays', label: 'Weekdays at 09:00' },
  { value: '@weekly', label: 'Every Sunday at 00:00' },
];

/** The select value of a custom cron expression. */
export const CUSTOM = 'custom';

/** The preset that a cron text is, or `custom`. */
export const presetOf = (cron: string): string =>
  PRESETS.some((p) => p.value === cron.trim()) ? cron.trim() : CUSTOM;

/**
 * What a cron text means, in its parts: a preset's label (words, sans), else the raw expression
 * (code, mono: DESIGN-SYSTEM §1 rule 5).
 */
export function describeCronParts(
  cron: string,
): { readonly label: string } | { readonly cron: string } {
  const preset = PRESETS.find((p) => p.value === cron.trim());
  return preset === undefined ? { cron: cron.trim() } : { label: preset.label };
}

/** What a cron text means as one text: a preset's label, else "cron" and the expression. */
export const describeCron = (cron: string): string => {
  const parts = describeCronParts(cron);
  return 'label' in parts ? parts.label : `cron ${parts.cron}`;
};

/** `+02:00` for the offset of the browser now (a fixed offset: the deck has no time zone database). */
export function browserOffset(date: Date = new Date()): string {
  const minutes = -date.getTimezoneOffset();
  if (minutes === 0) return 'UTC';
  const abs = Math.abs(minutes);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${minutes < 0 ? '-' : '+'}${pad(Math.floor(abs / 60))}:${pad(abs % 60)}`;
}

/** The form's state: every field is text or a flag. */
export type Draft = {
  name: string;
  enabled: boolean;
  preset: string;
  cron: string;
  timezone: string;
  station: string;
  path: string;
  instance: string;
  mode: string;
  model: string;
  prompt: string;
  precheck: string;
  reuse: boolean;
  graceMinutes: string;
};

export const DEFAULT_GRACE_MINUTES = 60;

export function emptyDraft(station = '', instance = ''): Draft {
  return {
    name: '',
    enabled: true,
    preset: '@weekdays',
    cron: '',
    timezone: browserOffset(),
    station,
    path: '',
    instance,
    mode: '',
    model: '',
    prompt: '',
    precheck: '',
    reuse: false,
    graceMinutes: String(DEFAULT_GRACE_MINUTES),
  };
}

export function draftOf(s: Schedule): Draft {
  const preset = presetOf(s.cron);
  return {
    name: s.name,
    enabled: s.enabled,
    preset,
    cron: preset === CUSTOM ? s.cron : '',
    timezone: s.timezone,
    station: s.station_id,
    path: s.path,
    instance: s.provider_instance,
    mode: s.mode ?? '',
    model: s.model ?? '',
    prompt: s.prompt,
    precheck: s.precheck_cmd ?? '',
    reuse: s.reuse_session,
    graceMinutes: String(Math.round(s.grace_ms / 60_000)),
  };
}

/** The deck's limits (mirrors `ScheduleInput::check`). */
export const MAX_NAME = 120;
export const MAX_PROMPT = 16_384;
export const MAX_PRECHECK = 4096;
const MAX_GRACE_MINUTES = 7 * 24 * 60;

/** The cron text of a draft. */
export const cronOf = (d: Draft): string => (d.preset === CUSTOM ? d.cron.trim() : d.preset);

/** Why a draft cannot be saved yet, or `null`. The deck checks the cron text itself. */
export function draftProblem(d: Draft): string | null {
  if (d.name.trim() === '') return 'Give the automation a name.';
  if (Array.from(d.name).length > MAX_NAME) return `A name has at most ${MAX_NAME} characters.`;
  if (d.station === '') return 'Choose a station.';
  if (d.path.trim() === '') return 'Enter the repository folder on the station.';
  if (d.instance === '') return 'Choose a provider.';
  if (d.prompt.trim() === '') return 'Write the prompt.';
  if (Array.from(d.prompt).length > MAX_PROMPT) {
    return `A prompt has at most ${MAX_PROMPT} characters.`;
  }
  if (cronOf(d) === '') return 'Enter a cron expression or choose a preset.';
  if (!/^(UTC|[+-]\d{2}:\d{2})$/.test(d.timezone.trim())) {
    return 'The time zone is UTC or an offset such as +02:00.';
  }
  if (d.precheck.trim() !== '') {
    if (Array.from(d.precheck).length > MAX_PRECHECK) {
      return `A precheck has at most ${MAX_PRECHECK} characters.`;
    }
    if (d.precheck.includes('\0')) return 'The precheck contains a NUL character.';
  }
  const grace = Number(d.graceMinutes);
  if (!Number.isInteger(grace) || grace < 1 || grace > MAX_GRACE_MINUTES) {
    return `The grace period is 1 to ${MAX_GRACE_MINUTES} minutes.`;
  }
  return null;
}

/** The body of a create or an update. Blank optional fields are left out. */
export function inputOf(d: Draft): ScheduleInput {
  const mode = d.mode.trim();
  const model = d.model.trim();
  return {
    name: d.name.trim(),
    enabled: d.enabled,
    cron: cronOf(d),
    timezone: d.timezone.trim(),
    station: d.station,
    path: d.path.trim(),
    provider_instance: d.instance,
    ...(mode === '' ? {} : { mode }),
    ...(model === '' ? {} : { model }),
    prompt: d.prompt,
    ...(d.precheck.trim() === '' ? {} : { precheck_cmd: d.precheck }),
    reuse_session: d.reuse,
    grace_ms: Number(d.graceMinutes) * 60_000,
  };
}

/** "in 2 h 10 min", "in 3 days", "due now". */
export function untilText(at: number, now: number): string {
  const minutes = Math.round((at - now) / 60_000);
  if (minutes <= 0) return 'due now';
  if (minutes < 60) return `in ${minutes} min`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) {
    const m = minutes % 60;
    return m === 0 ? `in ${hours} h` : `in ${hours} h ${m} min`;
  }
  const days = Math.floor(hours / 24);
  return days === 1 ? 'in 1 day' : `in ${days} days`;
}

export const STATE_LABEL: Readonly<Record<ScheduleRunState, string>> = {
  started: 'Started',
  skipped: 'Skipped',
  failed: 'Failed',
  missed: 'Missed',
};

/** The badge tone of a run state. */
export const STATE_TONE: Readonly<Record<ScheduleRunState, string>> = {
  started: 'ok',
  skipped: 'muted',
  failed: 'hazard',
  missed: 'act',
};

/** The line about the last run in the list: "Skipped · nothing to do". */
export function lastRunText(run: ScheduleRun | null): string {
  if (run === null) return 'Never run';
  const label = STATE_LABEL[run.state];
  return run.reason === '' ? label : `${label} · ${run.reason}`;
}
