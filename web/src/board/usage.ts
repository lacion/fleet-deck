// M3 6-9 (DL-303): usage and rate-limit meters. A part that no session
// reported is "unknown", never zero; a total over sessions with unknown
// parts says how many sessions it leaves out.
import type { DeckSession, StationView } from '../gen';
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

/** One rate-limit meter: the latest window of a scope of one provider
 * account (station + provider instance). */
export type RateMeter = {
  readonly key: string;
  readonly label: string;
  readonly scope: string;
  /** `null`: the provider did not say (unknown, not zero). */
  readonly usedPercent: number | null;
  readonly resetsAt: number | null;
};

/** The latest window per station, provider instance and scope, from the
 * most recently updated session that reported it. */
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
          label: `${stationName(s.station_id, stations)} · ${instance}`,
          scope: limit.scope,
          usedPercent: limit.used_percent,
          resetsAt: limit.resets_at,
        },
      });
    }
  }
  return [...latest.values()].map((v) => v.meter).sort((a, b) => a.key.localeCompare(b.key));
}

/** The text of a meter, for example "42% used, resets 2026-10-12 15:00". */
export function meterText(m: RateMeter): string {
  const used = m.usedPercent === null ? 'use unknown' : `${Math.round(m.usedPercent)}% used`;
  if (m.resetsAt === null) return used;
  const d = new Date(m.resetsAt);
  return `${used}, resets ${d.toISOString().slice(0, 16).replace('T', ' ')} UTC`;
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
