import { firstLine, isStartFailure } from '../app/failures';
import { formatHash } from '../app/routes';
import type { CardStatus, DeckSession } from '../gen';
import { MIN_SIBLINGS } from '../compare/compare';
import type { RunningTool } from '../gen';
import { errorSentence } from '../ui/format';
import {
  FileText,
  Globe,
  Icon,
  type LucideIcon,
  MessageSquare,
  Pencil,
  Plug,
  Search,
  Shield,
  SquareTerminal,
  Workflow,
  Wrench,
} from '../ui/icons';
import { isHibernated } from '../views/hibernate';
import { FocusLine, focusOf } from '../views/marks';
import { PushBadge, StaleBaseChip } from './PushBadge';

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
/** The statuses in which a strip shows the agent's focus line instead of the status note. */
const FOCUS_STATUSES: readonly CardStatus[] = [
  'working',
  'verifying',
  'needs_you',
  'queued',
  'idle',
];

/** The icon of a running tool, by its kind (the same icons as the chat's tool rows). */
const TOOL_ICON: Record<RunningTool['tool_kind'], LucideIcon> = {
  shell: SquareTerminal,
  edit: Pencil,
  read: FileText,
  search: Search,
  web: Globe,
  mcp: Plug,
  subagent: Workflow,
  other: Wrench,
};

function NoteLine({ card, focus }: { readonly card: DeckSession; readonly focus: boolean }) {
  const s = card.summary;
  const ask = s.open_requests[0];
  if (ask !== undefined) {
    return (
      <span className="fd-card-note">
        <span className="fd-card-ask" data-kind={ask.request_kind}>
          <Icon of={ask.request_kind === 'question' ? MessageSquare : Shield} size={12} />{' '}
          {ask.title}
        </span>
      </span>
    );
  }
  const tool = s.running_tools[0];
  if (tool !== undefined) {
    return (
      <span className="fd-card-note">
        <span className="fd-card-tool">
          <Icon of={TOOL_ICON[tool.tool_kind] ?? Wrench} size={12} />{' '}
          <span className="fd-card-tool-name">{tool.title}</span>
          {tool.input_summary === '' ? null : (
            <>
              {' '}
              <span className="fd-card-tool-arg">{tool.input_summary}</span>
            </>
          )}
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
  if (isHibernated(card)) {
    return (
      <span className="fd-card-note" data-note="hibernated">
        Hibernated · resumes with your next prompt
      </span>
    );
  }
  // The focus line is the agent's own words: only while the session is live.
  if (focus && FOCUS_STATUSES.includes(card.status) && focusOf(s) !== null) {
    return <FocusLine summary={s} />;
  }
  return (
    <span className="fd-card-note" data-note={card.status}>
      {STATUS_NOTE[card.status]}
    </span>
  );
}

/** The Fleet Git chips first (a narrow strip clips the end), then the note: "Stale base" and the push state (FG-1, FG-2). */
export function CardNote({
  card,
  focus = false,
}: {
  readonly card: DeckSession;
  readonly focus?: boolean;
}) {
  const git = card.summary.base_stale === true || card.summary.push !== undefined;
  return (
    <>
      {git ? (
        <span className="fd-card-gitchips">
          <StaleBaseChip card={card} /> <PushBadge card={card} />{' '}
        </span>
      ) : null}
      <NoteLine card={card} focus={focus} />
    </>
  );
}
