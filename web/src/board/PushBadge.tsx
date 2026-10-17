// FG-1, FG-2 (design §4.4, §5.9): the Fleet Git chips of a session card: "Stale base", and the
// push state ("Not pushed", "Pushing…", "Push stopped") with the "Push now" button.
import { useEffect, useState } from 'react';
import { untilText } from '../automations/automations';
import type { DeckSession, PushInfo } from '../gen';
import { useIntegrations } from '../integrations/context';
import { message } from '../integrations/useLoad';
import { stoppedTitle } from '../session/timeline_fleet_git';

/** A push that is due or running for longer than this shows "Pushing…". */
export const PUSHING_AFTER_MS = 60_000;

/** The title of "Not pushed" (§5.9). */
export function failingTitle(push: PushInfo, now: number): string {
  const next =
    push.next_attempt_at === undefined ? '' : ` Next try ${untilText(push.next_attempt_at, now)}.`;
  const times = push.attempts === 1 ? 'once' : `${push.attempts} times`;
  return `Push failed ${times}: ${push.text ?? ''}.${next}`;
}

/**
 * The badge that a push state shows on the card, or `null` for a state that shows nothing: a
 * stopped row without an unpublished commit, a `moved` or `move_failed` row, `idle`, `pushed`.
 */
export function pushBadgeOf(
  push: PushInfo,
  now: number,
  dueSince: number | null,
): {
  readonly kind: 'failing' | 'pushing' | 'stopped';
  readonly label: string;
  readonly title?: string;
} | null {
  switch (push.state) {
    case 'failing':
      return { kind: 'failing', label: 'Not pushed', title: failingTitle(push, now) };
    case 'due':
    case 'pushing':
      return dueSince !== null && now - dueSince > PUSHING_AFTER_MS
        ? { kind: 'pushing', label: 'Pushing…' }
        : null;
    case 'stopped': {
      if (push.stop_code === 'moved' || push.stop_code === 'move_failed') return null;
      if (push.desired_oid === undefined || push.desired_oid === push.oid) return null;
      const title = stoppedTitle(push);
      return { kind: 'stopped', label: 'Push stopped', ...(title === undefined ? {} : { title }) };
    }
    default:
      return null;
  }
}

/** The time at which the push became due or running, seen by this board; `null` while it is not. */
function useDueSince(active: boolean, override: number | undefined): number | null {
  const [since, setSince] = useState<number | null>(null);
  const [, tick] = useState(0);
  useEffect(() => {
    if (!active) {
      setSince(null);
      return undefined;
    }
    setSince((cur) => cur ?? Date.now());
    const timer = setTimeout(() => tick((n) => n + 1), PUSHING_AFTER_MS + 1000);
    return () => clearTimeout(timer);
  }, [active]);
  return override ?? since;
}

export function PushBadge({
  card,
  now,
  dueSince,
}: {
  readonly card: DeckSession;
  /** The clock (tests); absent: the time of the render. */
  readonly now?: number;
  /** When the push became due (tests); absent: the first time that this board saw it. */
  readonly dueSince?: number;
}) {
  const env = useIntegrations();
  const push = card.summary.push;
  const active = push?.state === 'due' || push?.state === 'pushing';
  const since = useDueSince(active, dueSince);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  if (push === undefined) return null;
  const clock = now ?? Date.now();
  const badge = pushBadgeOf(push, clock, since);
  if (badge === null) return null;
  const canPush = badge.kind === 'failing' && env?.actions.canDrive === true;
  const pushNow = (e: React.MouseEvent) => {
    // The card is a link: the button must not open the session.
    e.preventDefault();
    e.stopPropagation();
    if (busy || env === undefined) return;
    setBusy(true);
    setFailure(null);
    env.actions
      .pushNow(card.station_id, card.summary.session_id)
      .catch((err: unknown) => setFailure(message(err)))
      .finally(() => setBusy(false));
  };
  return (
    <>
      <span className="fd-badge" data-tone="hazard" data-push={badge.kind} title={badge.title}>
        {badge.label}
      </span>
      {canPush ? (
        <button type="button" className="fd-ghost fd-sm" disabled={busy} onClick={pushNow}>
          Push now
        </button>
      ) : null}
      {failure === null ? null : (
        <span className="fd-card-error" role="alert">
          {failure}
        </span>
      )}
    </>
  );
}

/** The chip "Stale base": the session started from the local HEAD (§4.4). */
export function StaleBaseChip({ card }: { readonly card: DeckSession }) {
  if (card.summary.base_stale !== true) return null;
  return (
    <span
      className="fd-badge"
      data-tone="hazard"
      data-chip="stale-base"
      title="The fetch failed; this session started from the local HEAD."
    >
      Stale base
    </span>
  );
}
