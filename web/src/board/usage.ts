// M3 6-9 (DL-303): usage and rate-limit meters. A part that no session
// reported is "unknown", never zero; a total over sessions with unknown
// parts says how many sessions it leaves out.
import type { DeckSession, StationView, Usage } from '../gen';
import { stationName } from './fleet';

/** A fleet total of one usage part. */
export type UsageTotal = {
  /** The sum of the known parts; `null` when no session reported one. */
  readonly known: number | null;
  /** Sessions that did not report this part. */
  readonly unknown: number;
};

export type FleetUsage = {
  readonly input: UsageTotal;
  readonly output: UsageTotal;
  readonly cost: UsageTotal;
  /** A part of `cost` is inferred from a price table (R4.6), not reported. */
  readonly costInferred: boolean;
};

function total(values: readonly (number | null)[]): UsageTotal {
  const known = values.filter((v): v is number => v !== null);
  return {
    known: known.length === 0 ? null : known.reduce((a, b) => a + b, 0),
    unknown: values.length - known.length,
  };
}

/** The usage of the shown sessions, summed per part. */
export function fleetUsage(sessions: readonly DeckSession[]): FleetUsage {
  const u = sessions.map((s) => s.summary.usage);
  return {
    input: total(u.map((x) => x.input_tokens)),
    output: total(u.map((x) => x.output_tokens)),
    cost: total(u.map((x) => x.cost_usd)),
    costInferred: u.some((x) => x.cost_usd !== null && x.cost_inferred),
  };
}

/** The text of a total: "unknown", the sum, or the sum and the gap. */
export function totalText(t: UsageTotal, format: (n: number) => string): string {
  if (t.known === null) return 'unknown';
  const sum = format(t.known);
  return t.unknown === 0
    ? sum
    : `at least ${sum} (${t.unknown} ${t.unknown === 1 ? 'session' : 'sessions'} unknown)`;
}

export const tokens = (n: number): string => n.toLocaleString('en-US');
export const dollars = (n: number): string => `$${n.toFixed(2)}`;

/** The cost of one session: "$1.20", or "≈ $1.20 inferred" (R4.6); `null` when unknown. */
export function costText(usage: Usage, format: (n: number) => string = dollars): string | null {
  if (usage.cost_usd === null) return null;
  const text = format(usage.cost_usd);
  return usage.cost_inferred ? `≈ ${text} inferred` : text;
}

/** The fleet cost total: "≈" in front when any part is inferred (R4.6). */
export function costTotalText(usage: FleetUsage): string {
  const text = totalText(usage.cost, dollars);
  return usage.costInferred ? `≈ ${text}` : text;
}

/** One rate-limit meter: the latest window of a scope of one provider
 * account (station + provider instance). */
export type RateMeter = {
  readonly key: string;
  readonly stationId: string;
  readonly instance: string;
  /** "Station · instance": the Usage tab, where every account has its own row. */
  readonly label: string;
  /** The provider display name of the instance: "Claude", "Codex", "Personal". */
  readonly provider: string;
  /** The station name (the glance shows it only when several stations report limits). */
  readonly station: string;
  readonly scope: string;
  /** `null`: the provider did not say (unknown, not zero). */
  readonly usedPercent: number | null;
  readonly resetsAt: number | null;
};

/** The latest window per station, provider instance and scope, from the
 * most recently updated session that reported it. The tightest window
 * (highest use) is first; a window with unknown use is last (R4.1). */
export function rateMeters(
  sessions: readonly DeckSession[],
  stations: readonly StationView[],
): RateMeter[] {
  const latest = new Map<string, { at: number; meter: RateMeter }>();
  for (const s of sessions) {
    const sum = s.summary;
    const instance = sum.provider_instance ?? sum.provider ?? 'unknown';
    for (const limit of sum.rate_limits) {
      const key = `${s.station_id}/${instance}/${limit.scope}`;
      const seen = latest.get(key);
      if (seen !== undefined && seen.at >= sum.updated_at) continue;
      latest.set(key, {
        at: sum.updated_at,
        meter: {
          key,
          stationId: s.station_id,
          instance,
          label: `${stationName(s.station_id, stations)} · ${instance}`,
          provider: providerName(instance),
          station: stationName(s.station_id, stations),
          scope: limit.scope,
          usedPercent: limit.used_percent,
          resetsAt: limit.resets_at,
        },
      });
    }
  }
  return [...latest.values()].map((v) => v.meter).sort(tighter);
}

/** The display name of a provider instance: "claude" → "Claude", "codex" → "Codex". */
export function providerName(instance: string): string {
  if (instance === 'claude') return 'Claude';
  if (instance === 'codex') return 'Codex';
  return instance.charAt(0).toUpperCase() + instance.slice(1);
}

/** The words of a rate-limit scope: "five_hour" → "5-hour window" (never the raw protocol scope). */
export const SCOPE_TEXT: Readonly<Record<string, string>> = {
  five_hour: '5-hour window',
  seven_day: 'weekly window',
  weekly: 'weekly window',
};

/** The words of a scope; a scope without words loses its underscores ("seven_day_opus" → "seven day opus"). */
export const scopeText = (scope: string): string => SCOPE_TEXT[scope] ?? scope.replace(/_/g, ' ');

/** Orders meters: highest use first, unknown use last, then by key. */
function tighter(a: RateMeter, b: RateMeter): number {
  if (a.usedPercent !== b.usedPercent) {
    if (a.usedPercent === null) return 1;
    if (b.usedPercent === null) return -1;
    return b.usedPercent - a.usedPercent;
  }
  return a.key.localeCompare(b.key);
}

const MINUTE_MS = 60_000;
const DAY_MS = 24 * 60 * MINUTE_MS;

/**
 * When a window resets: "resets in 2 h 10 min", "resets at 14:30" (local
 * time) when it is more than a day away, "reset due" when the time is past (R4.1).
 */
export function resetsText(resetsAt: number, now: number): string {
  const ms = resetsAt - now;
  if (ms <= 0) return 'reset due';
  if (ms > DAY_MS) {
    const d = new Date(resetsAt);
    const pad = (n: number) => String(n).padStart(2, '0');
    return `resets at ${pad(d.getHours())}:${pad(d.getMinutes())}`;
  }
  const minutes = Math.ceil(ms / MINUTE_MS);
  const h = Math.floor(minutes / 60);
  const m = minutes % 60;
  if (h === 0) return `resets in ${m} min`;
  return m === 0 ? `resets in ${h} h` : `resets in ${h} h ${m} min`;
}

/** The tightest window with a known use among the given meters, or `null`. */
export function tightest(meters: readonly RateMeter[]): RateMeter | null {
  return meters.find((m) => m.usedPercent !== null) ?? null;
}

/** The tightest window of one provider instance of one station, or `null`. */
export function instanceLimit(
  meters: readonly RateMeter[],
  stationId: string,
  instance: string,
): RateMeter | null {
  return tightest(meters.filter((m) => m.stationId === stationId && m.instance === instance));
}

/** From this use a limit warns (R4.2). */
export const WARN_PERCENT = 80;

/** The warning chip for the given meters: nothing under 80%, "Limit 85% · 5-hour window", or "Limit reached" at 100% (R4.2). */
export function limitChip(
  meters: readonly RateMeter[],
): { readonly tone: 'act' | 'hazard'; readonly text: string } | null {
  const m = tightest(meters);
  if (m === null || m.usedPercent === null || m.usedPercent < WARN_PERCENT) return null;
  if (m.usedPercent >= 100)
    return { tone: 'hazard', text: `Limit reached · ${scopeText(m.scope)}` };
  return { tone: 'act', text: `Limit ${Math.round(m.usedPercent)}% · ${scopeText(m.scope)}` };
}

/** The provider label of the spawn form with the limit of its instance (R4.3): "claude · 42% of 5-hour window, resets in 1 h". */
export function limitSuffix(m: RateMeter | null, now: number): string {
  if (m === null || m.usedPercent === null) return '';
  const resets = m.resetsAt === null ? '' : `, ${resetsText(m.resetsAt, now)}`;
  return `${Math.round(m.usedPercent)}% of ${scopeText(m.scope)}${resets}`;
}

/** The instance with the most room: the lowest tightest use; only when at least two instances report and the lowest is unique. */
export function mostRoom(limits: ReadonlyMap<string, RateMeter | null>): string | null {
  const known = [...limits].filter(
    (e): e is [string, RateMeter] => e[1] !== null && e[1].usedPercent !== null,
  );
  if (known.length < 2) return null;
  const used = (m: RateMeter) => m.usedPercent ?? 0;
  const least = Math.min(...known.map((e) => used(e[1])));
  const lowest = known.filter((e) => used(e[1]) === least);
  return lowest.length === 1 ? (lowest[0]?.[0] ?? null) : null;
}

/** The text of a meter, for example "42% used, resets in 2 h 10 min". */
export function meterText(m: RateMeter, now: number): string {
  const used = m.usedPercent === null ? 'use unknown' : `${Math.round(m.usedPercent)}% used`;
  return m.resetsAt === null ? used : `${used}, ${resetsText(m.resetsAt, now)}`;
}

/** The model-help use of one station today, under the "Fleet Deck" scope (R10, DL-167). */
export type AssistMeter = {
  readonly key: string;
  readonly label: string;
  readonly tokens: number;
  readonly cap: number;
};

/** One meter for each station whose model help is on. A station without a cap has it off. */
export function assistMeters(stations: readonly StationView[]): AssistMeter[] {
  const out: AssistMeter[] = [];
  for (const st of stations) {
    const info = st.info;
    const cap = info?.assist_cap;
    if (info == null || cap == null) continue;
    out.push({
      key: `${st.station_id}/fleetdeck`,
      label: `${st.name} · Fleet Deck`,
      tokens: info.assist_tokens_today ?? 0,
      cap,
    });
  }
  return out.sort((a, b) => a.key.localeCompare(b.key));
}

/** The text of a model-help meter, for example "1,200 of 200,000 tokens today". */
export function assistText(m: AssistMeter): string {
  return `${tokens(m.tokens)} of ${tokens(m.cap)} tokens today`;
}
