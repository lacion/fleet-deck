// The status lanes of the fleet board (PLAN §11). The lane of a session is
// its `activity`, which the deck projects from the canonical events.
import type { Activity, ProviderKind, SessionSummary } from '../gen';

export type Lane = {
  readonly activity: Activity;
  readonly label: string;
};

// Left to right: what needs the owner first. `satisfies` makes the
// typecheck fail when the Rust `Activity` enum gains a variant.
const LABELS = {
  needs_you: 'Needs you',
  working: 'Working',
  background: 'Background',
  idle: 'Idle',
  ended: 'Ended',
} as const satisfies Record<Activity, string>;

export const LANES: readonly Lane[] = (Object.keys(LABELS) as Activity[]).map((activity) => ({
  activity,
  label: LABELS[activity],
}));

export type LaneGroups = Record<Activity, SessionSummary[]>;

/** Groups sessions by lane; inside a lane the newest update comes first. */
export function groupByLane(sessions: readonly SessionSummary[]): LaneGroups {
  const groups: LaneGroups = { needs_you: [], working: [], background: [], idle: [], ended: [] };
  for (const session of sessions) {
    groups[session.activity].push(session);
  }
  for (const lane of LANES) {
    groups[lane.activity].sort(
      (a, b) => b.updated_at - a.updated_at || a.session_id.localeCompare(b.session_id),
    );
  }
  return groups;
}

/** The text of the provider badge. */
export function providerLabel(provider: ProviderKind | null): string {
  switch (provider) {
    case 'claude':
      return 'Claude';
    case 'codex':
      return 'Codex';
    case 'unknown':
    case null:
      return 'Unknown';
  }
}
