// M3 6-6 (DL-301): the plan library. The deck keeps each plan that a
// session proposed (Claude `ExitPlanMode`, Codex plan items); the board
// lists them, shows the text, sets the owner's status and starts a new
// session with a plan as its first prompt.
import type { DeckPlan, DeckSession, PlanStatus } from '../gen';
import type { SpawnActions } from '../spawn/spawn';

type Get = (path: string) => Promise<unknown>;
type Post = (path: string, body: unknown) => Promise<unknown>;

export type PlanActions = {
  readonly canDrive: boolean;
  list(archived: boolean): Promise<DeckPlan[]>;
  mark(id: string, status: PlanStatus): Promise<DeckPlan>;
};

const STATUSES: readonly string[] = ['proposed', 'executed', 'archived'];

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

/** Validates one library row from the deck. */
export function planView(v: unknown): DeckPlan {
  if (
    !record(v) ||
    typeof v['id'] !== 'string' ||
    typeof v['station_id'] !== 'string' ||
    typeof v['session_id'] !== 'string' ||
    typeof v['plan_id'] !== 'string' ||
    typeof v['markdown'] !== 'string' ||
    typeof v['truncated'] !== 'boolean' ||
    typeof v['status'] !== 'string' ||
    !STATUSES.includes(v['status']) ||
    typeof v['proposed_at'] !== 'number'
  ) {
    throw new Error('The deck sent an invalid plan.');
  }
  return v as DeckPlan;
}

/** The plan library calls (`GET /api/plans`, `POST /api/plans/mark`). */
export function planActions(get: Get, post: Post, scopes: readonly string[]): PlanActions {
  return {
    canDrive: scopes.includes('drive'),
    list: async (archived) => {
      const v = await get(archived ? '/api/plans?archived=true' : '/api/plans');
      if (!Array.isArray(v)) throw new Error('The deck sent an invalid plan list.');
      return v.map(planView);
    },
    mark: async (id, status) => planView(await post('/api/plans/mark', { id, status })),
  };
}

/** The title of a plan: its first heading, else its first non-empty line. */
export function planTitle(markdown: string): string {
  const lines = markdown
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l !== '');
  const heading = lines.find((l) => l.startsWith('#'));
  const text = (heading ?? lines[0] ?? '').replace(/^#+\s*/, '');
  if (text === '') return 'Untitled plan';
  return text.length > 80 ? `${text.slice(0, 79)}…` : text;
}

export const STATUS_LABEL: Readonly<Record<PlanStatus, string>> = {
  proposed: 'Proposed',
  executed: 'Executed',
  archived: 'Archived',
};

/** The live session that proposed a plan, or `undefined` (R7.2). */
export function liveProposer(
  plan: DeckPlan,
  sessions: readonly DeckSession[],
): DeckSession | undefined {
  return sessions.find(
    (c) =>
      c.station_id === plan.station_id &&
      c.summary.session_id === plan.session_id &&
      c.status !== 'ended' &&
      c.status !== 'lost' &&
      c.status !== 'unreachable',
  );
}

/**
 * The note under a proposed plan whose proposer session still runs: the text before and after the
 * proposer's callsign, which the library shows as a `.fd-callsign`.
 */
export const PROPOSER_NOTE = {
  before: 'The proposer session ',
  after: ' is still running; this starts a new session.',
} as const;

/**
 * `spawn`, where a start that returned ok also marks the plan `executed` (R7.2). A failed start
 * leaves the plan `proposed`. A failed mark goes to `onMarkError`; the session still started.
 */
export function markOnStart(
  spawn: SpawnActions,
  plans: PlanActions,
  planId: string,
  onMarkError: (e: unknown) => void,
): SpawnActions {
  return {
    ...spawn,
    start: async (...args) => {
      const outcome = await spawn.start(...args);
      if (outcome.ok) plans.mark(planId, 'executed').catch(onMarkError);
      return outcome;
    },
  };
}
