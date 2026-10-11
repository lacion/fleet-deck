import type { CardStatus, DeckSession, ProviderKind, StationView } from '../gen';
import { sessionKey } from '../runtime/state';

export type FleetFilter = {
  readonly station: string;
  readonly provider: string;
  readonly search: string;
};
export const ALL = '';
export const emptyFilter: FleetFilter = { station: ALL, provider: ALL, search: '' };

const labels = {
  needs_you: 'Needs you',
  queued: 'Queued',
  working: 'Working',
  verifying: 'Verifying',
  idle: 'Idle',
  ended: 'Ended',
  unreachable: 'Unreachable',
  lost: 'Lost',
} as const satisfies Record<CardStatus, string>;
export const STATUS_LANES = (Object.keys(labels) as CardStatus[]).map((status) => ({
  status,
  label: labels[status],
}));
export const cardKey = (card: DeckSession): string =>
  sessionKey({ station_id: card.station_id, session_id: card.summary.session_id });
export function stationName(id: string, stations: readonly StationView[]): string {
  return stations.find((station) => station.station_id === id)?.name ?? id;
}

/** Filters never change the source projections or the cached runtime state. */
export function filterFleet(
  sessions: readonly DeckSession[],
  stations: readonly StationView[],
  filter: FleetFilter,
): DeckSession[] {
  const search = filter.search.trim().toLowerCase();
  return sessions.filter((card) => {
    const s = card.summary;
    if (filter.station && card.station_id !== filter.station) return false;
    if (filter.provider && (s.provider ?? 'unknown') !== filter.provider) return false;
    const text = [
      s.title,
      s.session_id,
      card.station_id,
      stationName(card.station_id, stations),
      s.cwd,
      s.model,
      s.provider_instance,
    ].join('\n');
    return !search || text.toLowerCase().includes(search);
  });
}

/** Use the deck status, not the last station activity: link loss can change it. */
export function statusGroups(sessions: readonly DeckSession[]): Record<CardStatus, DeckSession[]> {
  const groups: Record<CardStatus, DeckSession[]> = {
    needs_you: [],
    queued: [],
    working: [],
    verifying: [],
    idle: [],
    ended: [],
    unreachable: [],
    lost: [],
  };
  for (const card of sessions) groups[card.status].push(card);
  for (const { status } of STATUS_LANES) {
    groups[status].sort(
      (a, b) => b.summary.updated_at - a.summary.updated_at || cardKey(a).localeCompare(cardKey(b)),
    );
  }
  return groups;
}

/** Observed session data is not the configured provider inventory. */
export function observedProviders(sessions: readonly DeckSession[], station: string): string[] {
  const providers = new Set<string>();
  for (const card of sessions) {
    if (card.station_id !== station) continue;
    const s = card.summary;
    providers.add(
      `${providerText(s.provider)} · ${s.provider_instance ?? 'instance unknown'} · ${s.provider_version ?? 'version unknown'}`,
    );
  }
  return [...providers].sort();
}
function providerText(kind: ProviderKind | null): string {
  return kind === 'claude' ? 'Claude' : kind === 'codex' ? 'Codex' : 'Unknown';
}
export function lastSeen(value: number | null): string {
  if (value === null) return 'Never';
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? 'Unknown' : date.toISOString();
}
export function usageText(card: DeckSession): string {
  const usage = card.summary.usage;
  return `Input ${usage.input_tokens ?? 'unknown'} · Output ${usage.output_tokens ?? 'unknown'} · Cost ${usage.cost_usd === null ? 'unknown' : `$${usage.cost_usd.toFixed(4)}`}`;
}
