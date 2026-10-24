// B29: the sums of the analytics page (design analytics.md §3.10 "Merge"). Pure functions: the
// answer of the deck goes in, the totals, the local days and the inputs of the learnings come out.
import type {
  ActivityHour,
  AnalyticsSource,
  FleetAnalytics,
  StationAnalytics,
  UsageHour,
} from '../gen';

/** One hour in ms. `since` and the hour rows are multiples of it. */
export const HOUR_MS = 3_600_000;
const DAY_MS = 24 * HOUR_MS;
/** The longest window that also gets one total per hour (the past 24 hours). */
const HOURLY_WINDOW_MS = 25 * HOUR_MS;

/** Days; 1 = the past 24 hours; 'all' = `since` 0. */
export type Period = 1 | 7 | 30 | 90 | 365 | 'all';

export type Totals = {
  readonly input: number;
  readonly cacheRead: number;
  readonly cacheWrite: number;
  readonly output: number;
  readonly reasoning: number;
  readonly requests: number;
  /** Sum of the priced buckets. */
  readonly cost: number;
  /** Tokens of buckets without a price. */
  readonly unpricedTokens: number;
  readonly unpricedModels: readonly string[];
};

export type Day = {
  /** `YYYY-MM-DD` of the browser's zone (an hour entry: `YYYY-MM-DD HH`). */
  readonly key: string;
  readonly start: number;
  readonly totals: Totals;
};

export type StationSums = {
  readonly id: string;
  readonly name: string;
  readonly state: string;
  readonly totals: Totals | null;
  readonly pending: number | null;
  readonly complete: boolean;
  readonly truncatedBefore: number | null;
  readonly partialHours: readonly number[];
};

export type SessionSums = { readonly turnsEnded: number; readonly totals: Totals };

export type Merged = {
  readonly total: Totals;
  /** Rows without a session (not in a proven Fleet Deck run); null for a member. */
  readonly notInRun: Totals | null;
  /** Every local day of the window, oldest first. */
  readonly days: readonly Day[];
  /** One entry per hour of the window, oldest first; empty when the window is longer than 25 hours. */
  readonly hours: readonly Day[];
  /** By tokens, largest first. */
  readonly models: readonly { provider: string; model: string; totals: Totals }[];
  readonly stations: readonly StationSums[];
  /**
   * The earliest `first_at` of the counted sources (member form: already clipped to
   * `events_since` by the station); days before its local day have no data.
   */
  readonly usageSince: number | null;
  /** Per session, from `learningSince` on: turns that ended and the attributed usage (§3.12). */
  readonly perSession: ReadonlyMap<string, SessionSums>;
  /**
   * The start of the period of `perSession`: `max(window since, latest events_since, usageSince)`.
   * Rows of the hours from the first whole hour at or after it are in `perSession`.
   */
  readonly learningSince: number;
  /** Every counted source is `ok` and `complete`, and no station answer is cut. */
  readonly usageComplete: boolean;
  /** Metric key → sum. */
  readonly counts: ReadonlyMap<string, number>;
  /** The latest `events_since` of the stations that answered. */
  readonly eventsSince: number | null;
  /** The earliest `kept_since`. */
  readonly keptSince: number | null;
  readonly priceTables: readonly string[];
  /** "Claude on thunder (same home as Lighting)". */
  readonly duplicateSources: readonly string[];
  readonly window: { readonly since: number; readonly until: number };
};

// ── the window ──

/** `since` is an hour start (UTC ms); `until` is now. `all`: `since` 0. */
export function windowOf(period: Period, now: number): { since: number; until: number } {
  if (period === 'all') return { since: 0, until: now };
  const from = now - period * DAY_MS;
  return { since: Math.floor(from / HOUR_MS) * HOUR_MS, until: now };
}

// ── local days ──

type Parts = { y: number; m: number; d: number; h: number };

const formatters = new Map<string, Intl.DateTimeFormat>();

function formatter(zone: string): Intl.DateTimeFormat {
  let f = formatters.get(zone);
  if (f === undefined) {
    f = new Intl.DateTimeFormat('en-US', {
      timeZone: zone,
      hourCycle: 'h23',
      year: 'numeric',
      month: 'numeric',
      day: 'numeric',
      hour: 'numeric',
      minute: 'numeric',
      second: 'numeric',
    });
    formatters.set(zone, f);
  }
  return f;
}

/** The wall clock of `ts` in `zone`, as UTC ms (so the zone offset is the difference). */
function wallAsUtc(ts: number, zone: string): { utc: number; parts: Parts } {
  const p: Record<string, number> = {};
  for (const part of formatter(zone).formatToParts(new Date(ts))) {
    if (part.type !== 'literal') p[part.type] = Number(part.value);
  }
  const y = p['year'] ?? 1970;
  const m = p['month'] ?? 1;
  const d = p['day'] ?? 1;
  const h = (p['hour'] ?? 0) % 24;
  return {
    utc: Date.UTC(y, m - 1, d, h, p['minute'] ?? 0, p['second'] ?? 0),
    parts: { y, m, d, h },
  };
}

const two = (n: number): string => String(n).padStart(2, '0');
const dayKey = (y: number, m: number, d: number): string => `${y}-${two(m)}-${two(d)}`;

/** The ms of the start of a local day. */
function localMidnight(y: number, m: number, d: number, zone: string): number {
  const wall = Date.UTC(y, m - 1, d);
  let t = wall - (wallAsUtc(wall, zone).utc - wall);
  t = wall - (wallAsUtc(t, zone).utc - t);
  return t;
}

// ── sums ──

type Acc = {
  input: number;
  cacheRead: number;
  cacheWrite: number;
  output: number;
  reasoning: number;
  requests: number;
  cost: number;
  unpricedTokens: number;
  models: Set<string>;
};

const newAcc = (): Acc => ({
  input: 0,
  cacheRead: 0,
  cacheWrite: 0,
  output: 0,
  reasoning: 0,
  requests: 0,
  cost: 0,
  unpricedTokens: 0,
  models: new Set(),
});

/** Tokens of a total: the four parts; reasoning is a part of output and is never added. */
export const tokensOf = (
  t: Pick<Totals, 'input' | 'cacheRead' | 'cacheWrite' | 'output'>,
): number => t.input + t.cacheRead + t.cacheWrite + t.output;

export const EMPTY_TOTALS: Totals = finish(newAcc());

function add(acc: Acc, row: UsageHour): void {
  acc.input += row.input;
  acc.cacheRead += row.cache_read;
  acc.cacheWrite += row.cache_write;
  acc.output += row.output;
  acc.reasoning += row.reasoning;
  acc.requests += row.requests;
  if (row.cost_usd === undefined) {
    acc.unpricedTokens += row.input + row.cache_read + row.cache_write + row.output;
    acc.models.add(row.model);
  } else {
    acc.cost += row.cost_usd;
  }
}

function finish(acc: Acc): Totals {
  return {
    input: acc.input,
    cacheRead: acc.cacheRead,
    cacheWrite: acc.cacheWrite,
    output: acc.output,
    reasoning: acc.reasoning,
    requests: acc.requests,
    cost: acc.cost,
    unpricedTokens: acc.unpricedTokens,
    unpricedModels: [...acc.models].sort(),
  };
}

const byName = (a: { name: string }, b: { name: string }): number =>
  a.name < b.name ? -1 : a.name > b.name ? 1 : 0;

const PROVIDER_LABEL: Record<string, string> = { claude: 'Claude', codex: 'Codex' };

type Group = { rows: UsageHour[]; requests: number; hasSession: boolean };
type Counted = { row: UsageHour; station: number };

/** The ended turns of an activity row. */
const ended = (counts: Readonly<Record<string, number>>): number =>
  (counts['turns.done'] ?? 0) + (counts['turns.interrupted'] ?? 0) + (counts['turns.failed'] ?? 0);

/** True when every counted source of the answer is read to its end (§3.12 `usageComplete`). */
function stationComplete(s: StationAnalytics): boolean {
  if (s.state !== 'ok' || s.answer === undefined) return false;
  if (s.truncated_before !== undefined || (s.partial_hours?.length ?? 0) > 0) return false;
  return countedSources(s.answer.sources).every((src) => src.state === 'ok' && src.complete);
}

/** A home that does not exist on the station has nothing to count and is left out. */
const countedSources = (sources: readonly AnalyticsSource[]): AnalyticsSource[] =>
  sources.filter((src) => src.state !== 'missing');

type Entry = { station: number; group: Group };

const NUMBERS = ['requests', 'input', 'cache_read', 'cache_write', 'output', 'reasoning'] as const;

/**
 * The rows that count for one bucket identity `(source key, hour, provider, model, conversation)`
 * (design §3.10 "One home read by two stations", build answer A11). `entries` are the station
 * groups of the identity, the stations sorted by name.
 *
 * - A proven row (with a `session_id`) holds only the records of that session's runs on its
 *   station, so the proven rows of different sessions are disjoint and all count. The rows of one
 *   session on two stations (one record set seen twice) count once: the group with the most
 *   requests (a tie: the first station by name).
 * - The unproven rest: the winner group (the most requests; a tie: the group with a session,
 *   then the first station by name) holds the most complete count of the bucket. The part of it
 *   that the counted proven rows do not hold counts once, as one row without a session on the
 *   winner's station (the owner only: a member answer has no unproven row).
 * - Two stations that prove one record for two sessions (runs at one time) make the proven rows
 *   larger than the winner: for the owner, the winner group alone counts (no double count).
 */
function countBucket(entries: readonly Entry[], owner: boolean): Counted[] {
  let win: Entry | undefined;
  for (const e of entries) {
    const better =
      win === undefined ||
      e.group.requests > win.group.requests ||
      (e.group.requests === win.group.requests && e.group.hasSession && !win.group.hasSession);
    if (better) win = e;
  }
  if (win === undefined) return [];
  if (entries.length === 1) return win.group.rows.map((row) => ({ row, station: win.station }));

  const bySession = new Map<string, { station: number; rows: UsageHour[]; requests: number }>();
  for (const { station, group } of entries) {
    const mine = new Map<string, { station: number; rows: UsageHour[]; requests: number }>();
    for (const row of group.rows) {
      if (row.session_id === undefined) continue;
      const v = mine.get(row.session_id) ?? { station, rows: [], requests: 0 };
      v.rows.push(row);
      v.requests += row.requests;
      mine.set(row.session_id, v);
    }
    for (const [id, v] of mine) {
      const best = bySession.get(id);
      if (best === undefined || v.requests > best.requests) bySession.set(id, v);
    }
  }
  const proven: Counted[] = [];
  for (const v of bySession.values()) {
    for (const row of v.rows) proven.push({ row, station: v.station });
  }
  if (!owner) return proven;

  const provenRequests = proven.reduce((n, c) => n + c.row.requests, 0);
  if (provenRequests > win.group.requests) {
    return win.group.rows.map((row) => ({ row, station: win.station }));
  }
  const first = win.group.rows[0];
  if (first === undefined) return proven;
  const rest: UsageHour = { ...first };
  delete rest.session_id;
  for (const key of NUMBERS) {
    const all = win.group.rows.reduce((n, r) => n + r[key], 0);
    const known = proven.reduce((n, c) => n + c.row[key], 0);
    rest[key] = Math.max(0, all - known);
  }
  const priced = win.group.rows.filter((r) => r.cost_usd !== undefined);
  if (priced.length === 0) {
    delete rest.cost_usd;
  } else {
    const all = priced.reduce((n, r) => n + (r.cost_usd ?? 0), 0);
    const known = proven.reduce((n, c) => n + (c.row.cost_usd ?? 0), 0);
    rest.cost_usd = Math.max(0, all - known);
  }
  const empty = NUMBERS.every((key) => rest[key] === 0);
  return empty ? proven : [...proven, { row: rest, station: win.station }];
}

export function merge(answer: FleetAnalytics, since: number, until: number, zone: string): Merged {
  const answering = answer.stations
    .filter((s) => s.state === 'ok' && s.answer !== undefined)
    .sort(byName);

  // One home read by two stations (revision 2, build answer A11): per bucket identity, the
  // proven rows of every station count once per session, and the unproven rest of the most
  // complete station group counts once (`countBucket`).
  const buckets = new Map<string, { station: number; group: Group }[]>();
  answering.forEach((s, station) => {
    const sources = s.answer?.sources ?? [];
    const map = new Map<string, Group>();
    for (const row of s.answer?.usage ?? []) {
      if (row.hour < since || row.hour >= until) continue;
      const src = sources[row.source];
      if (src === undefined) continue;
      const id = `${src.key}|${row.hour}|${row.provider}|${row.model}|${row.conversation}`;
      let g = map.get(id);
      if (g === undefined) {
        g = { rows: [], requests: 0, hasSession: false };
        map.set(id, g);
      }
      g.rows.push(row);
      g.requests += row.requests;
      g.hasSession = g.hasSession || row.session_id !== undefined;
    }
    for (const [id, group] of map) {
      const list = buckets.get(id) ?? [];
      list.push({ station, group });
      buckets.set(id, list);
    }
  });
  const counted: Counted[] = [];
  for (const entries of buckets.values()) counted.push(...countBucket(entries, answer.owner));

  const owner = answer.owner;
  const total = newAcc();
  const notInRun = newAcc();
  const stationAcc = answering.map(() => newAcc());
  const modelAcc = new Map<string, { provider: string; model: string; acc: Acc }>();
  const hourAcc = new Map<number, Acc>();
  const dayAcc = new Map<string, Acc>();
  const keyOfHour = new Map<number, { key: string; wall: Parts }>();
  const local = (hour: number): { key: string; wall: Parts } => {
    let v = keyOfHour.get(hour);
    if (v === undefined) {
      const { parts } = wallAsUtc(hour, zone);
      v = { key: dayKey(parts.y, parts.m, parts.d), wall: parts };
      keyOfHour.set(hour, v);
    }
    return v;
  };
  const into = <K>(map: Map<K, Acc>, key: K, row: UsageHour): void => {
    let a = map.get(key);
    if (a === undefined) {
      a = newAcc();
      map.set(key, a);
    }
    add(a, row);
  };

  let earliestRow: number | null = null;
  for (const { row, station } of counted) {
    add(total, row);
    if (row.session_id === undefined) add(notInRun, row);
    add(stationAcc[station] ?? newAcc(), row);
    const mk = `${row.provider}|${row.model}`;
    let m = modelAcc.get(mk);
    if (m === undefined) {
      m = { provider: row.provider, model: row.model, acc: newAcc() };
      modelAcc.set(mk, m);
    }
    add(m.acc, row);
    into(hourAcc, row.hour, row);
    into(dayAcc, local(row.hour).key, row);
    if (earliestRow === null || row.hour < earliestRow) earliestRow = row.hour;
  }

  // Sources, coverage, counters.
  let usageSince: number | null = null;
  let eventsSince: number | null = null;
  let keptSince: number | null = null;
  const priceTables: string[] = [];
  const counts = new Map<string, number>();
  const homes = new Map<string, string[]>();
  for (const s of answering) {
    const a = s.answer;
    if (a === undefined) continue;
    for (const src of a.sources) {
      if (
        src.state === 'ok' &&
        src.first_at !== null &&
        (usageSince === null || src.first_at < usageSince)
      ) {
        usageSince = src.first_at;
      }
      if (src.state !== 'missing') {
        const id = `${src.provider}|${src.key}`;
        const names = homes.get(id) ?? [];
        if (!names.includes(s.name)) names.push(s.name);
        homes.set(id, names);
      }
    }
    const { events_since: e, kept_since: k } = a.coverage;
    if (e !== null && (eventsSince === null || e > eventsSince)) eventsSince = e;
    if (k !== null && (keptSince === null || k < keptSince)) keptSince = k;
    if (!priceTables.includes(a.price_table)) priceTables.push(a.price_table);
    for (const act of a.activity) {
      for (const [key, n] of Object.entries(act.counts))
        counts.set(key, (counts.get(key) ?? 0) + n);
    }
  }
  const duplicateSources: string[] = [];
  for (const [id, names] of homes) {
    const first = names[0];
    if (names.length < 2 || first === undefined) continue;
    const provider = id.slice(0, id.indexOf('|'));
    const label = PROVIDER_LABEL[provider] ?? 'Provider';
    for (const name of names.slice(1))
      duplicateSources.push(`${label} on ${name} (same home as ${first})`);
  }
  duplicateSources.sort();

  // The period of the learnings: from the first whole hour at or after its start.
  const learningSince = Math.max(since, eventsSince ?? 0, usageSince ?? 0);
  const learningHour = Math.ceil(learningSince / HOUR_MS) * HOUR_MS;
  const sessionAcc = new Map<string, { turns: number; acc: Acc }>();
  const session = (id: string): { turns: number; acc: Acc } => {
    let v = sessionAcc.get(id);
    if (v === undefined) {
      v = { turns: 0, acc: newAcc() };
      sessionAcc.set(id, v);
    }
    return v;
  };
  for (const { row } of counted) {
    if (row.session_id !== undefined && row.hour >= learningHour)
      add(session(row.session_id).acc, row);
  }
  for (const s of answering) {
    for (const act of s.answer?.activity ?? ([] as ActivityHour[])) {
      if (act.hour >= learningHour && act.hour < until)
        session(act.session_id).turns += ended(act.counts);
    }
  }
  const perSession = new Map<string, SessionSums>();
  for (const [id, v] of sessionAcc)
    perSession.set(id, { turnsEnded: v.turns, totals: finish(v.acc) });

  // Local days (and hours for a window of a day).
  const days: Day[] = [];
  const first = since > 0 ? since : minOf(usageSince, earliestRow);
  if (first !== null && first < until) {
    const lastKey = local(until - 1).key;
    let { y, m, d } = local(first).wall;
    for (let i = 0; i < 20_000; i += 1) {
      const key = dayKey(y, m, d);
      if (key > lastKey) break;
      const acc = dayAcc.get(key);
      days.push({
        key,
        start: localMidnight(y, m, d, zone),
        totals: acc ? finish(acc) : EMPTY_TOTALS,
      });
      const next = new Date(Date.UTC(y, m - 1, d + 1));
      y = next.getUTCFullYear();
      m = next.getUTCMonth() + 1;
      d = next.getUTCDate();
    }
  }
  const hours: Day[] = [];
  if (since > 0 && until - since <= HOURLY_WINDOW_MS) {
    for (let start = since; start < until; start += HOUR_MS) {
      const acc = hourAcc.get(start);
      const { wall, key } = local(start);
      hours.push({
        key: `${key} ${two(wall.h)}`,
        start,
        totals: acc ? finish(acc) : EMPTY_TOTALS,
      });
    }
  }

  const models = [...modelAcc.values()]
    .map((v) => ({ provider: v.provider, model: v.model, totals: finish(v.acc) }))
    .sort((a, b) => tokensOf(b.totals) - tokensOf(a.totals) || (a.model < b.model ? -1 : 1));

  const index = new Map(answering.map((s, i) => [s.station_id, i]));
  const stations: StationSums[] = [...answer.stations].sort(byName).map((s) => {
    const i = index.get(s.station_id);
    const sources = s.answer?.sources ?? [];
    const pending = sources.filter((src) => src.pending_bytes !== null);
    return {
      id: s.station_id,
      name: s.name,
      state: s.state,
      totals: i === undefined ? null : finish(stationAcc[i] ?? newAcc()),
      pending:
        pending.length === 0
          ? null
          : pending.reduce((sum, src) => sum + (src.pending_bytes ?? 0), 0),
      complete: stationComplete(s),
      truncatedBefore: s.truncated_before ?? null,
      partialHours: s.partial_hours ?? [],
    };
  });

  const usageComplete =
    answering.some((s) => countedSources(s.answer?.sources ?? []).length > 0) &&
    answering.every(stationComplete);

  return {
    total: finish(total),
    notInRun: owner ? finish(notInRun) : null,
    days,
    hours,
    models,
    stations,
    usageSince,
    perSession,
    learningSince,
    usageComplete,
    counts,
    eventsSince,
    keptSince,
    priceTables,
    duplicateSources,
    window: { since, until },
  };
}

function minOf(a: number | null, b: number | null): number | null {
  if (a === null) return b;
  if (b === null) return a;
  return Math.min(a, b);
}
