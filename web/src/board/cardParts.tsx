import { firstLine, isStartFailure } from '../app/failures';
import { formatHash } from '../app/routes';
import type { CardStatus, DeckSession } from '../gen';
import { MIN_SIBLINGS } from '../compare/compare';
import { errorSentence } from '../ui/format';
import { Icon, TriangleAlert } from '../ui/icons';
import { isHibernated } from '../views/hibernate';
import { FocusLine, focusOf } from '../views/marks';

/** The chip under a batch card (R1.4): a link to the compare page; the card is a link too, so it sits beside it. */
export function CompareChip({
  card,
  count,
}: {
  readonly card: DeckSession;
  readonly count: number;
}) {
  const batch = card.summary.batch;
  if (batch === undefined || count < MIN_SIBLINGS) return null;
  return (
    <a
      className="fd-card-compare"
      href={formatHash({ name: 'compare', batch: batch.batch_id })}
      title="Compare the sessions of this batch side by side"
    >
      Compare {count}
    </a>
  );
}

export const STATUS_NOTE: Record<CardStatus, string> = {
  queued: 'Queued',
  working: 'Working',
  verifying: 'Verifying',
  needs_you: 'Needs you',
  idle: 'Waiting for a prompt',
  ended: 'Ended',
  unreachable: 'Station unreachable',
  lost: 'Lost',
};

/**
 * The one line of what a session does now: an open ask, a running tool, a failure, then (with
 * `focus`, on the strip) the agent focus line, then hibernated, then the status.
 */
export function CardNote({
  card,
  focus = false,
}: {
  readonly card: DeckSession;
  readonly focus?: boolean;
}) {
  const s = card.summary;
  const ask = s.open_requests[0];
  if (ask !== undefined) {
    return (
      <span className="fd-card-note">
        <span className="fd-card-ask">
          <Icon of={TriangleAlert} size={12} /> {ask.title}
        </span>
      </span>
    );
  }
  const tool = s.running_tools[0];
  if (tool !== undefined) {
    return (
      <span className="fd-card-note">
        <span className="fd-card-tool">
          {tool.input_summary === '' ? tool.title : `${tool.title}: ${tool.input_summary}`}
        </span>
      </span>
    );
  }
  const error = s.last_error;
  if (error != null && isStartFailure(card)) {
    return (
      <span className="fd-card-note" data-note="start_failed">
        <span className="fd-card-error">Failed to start: {firstLine(error.message)}</span>
      </span>
    );
  }
  if (
    error !== null &&
    error !== undefined &&
    (card.status === 'idle' || card.status === 'ended' || card.status === 'lost')
  ) {
    return (
      <span className="fd-card-note">
        <span className="fd-card-error">{errorSentence(`${error.code}: ${error.message}`)}</span>
      </span>
    );
  }
  if (focus && focusOf(s) !== null) return <FocusLine summary={s} />;
  if (isHibernated(card)) {
    return (
      <span className="fd-card-note" data-note="hibernated">
        Hibernated · resumes with your next prompt
      </span>
    );
  }
  return (
    <span className="fd-card-note" data-note={card.status}>
      {STATUS_NOTE[card.status]}
    </span>
  );
}
