// The error rows of the transcript (DESIGN-SYSTEM §4.7 notices): a failed turn and a station error
// are one notice row each, in the error tone (the octagon-alert icon and the hazard holder), so
// that a failure reads as the strongest line of the turn and never by its color alone. The quiet
// notices stay a faint line.
import { countText } from '../ui/format';
import { ChevronRight, Icon, OctagonAlert } from '../ui/icons';
import { apiCause, type NoticeItem, TURN_TEXT } from './notices';

/** The count of a notice that came more than once in a row: a pill after the text (§5), with its
    word for a screen reader ("2 times"). */
function Repeat({ count }: { readonly count: number | undefined }) {
  if (count === undefined || count < 2) return null;
  return (
    <span className="fd-count">
      {countText(count)}
      <span className="fd-vh"> times</span>
    </span>
  );
}

/**
 * A notice of the timeline: an error is a notice row, any other one a quiet line. The count is a
 * flex item after the text, so that it never wraps alone onto a line of its own.
 */
export function NoticeLine({ item }: { readonly item: NoticeItem }) {
  if (item.tone !== 'error') {
    return (
      <p className="fd-chat-line" title={item.title}>
        {item.text}
        <Repeat count={item.repeat} />
      </p>
    );
  }
  return (
    <div className="fd-notice fd-chat-alert" data-tone="error" data-place="page" title={item.title}>
      <Icon of={OctagonAlert} size={14} />
      <p className="fd-notice-text">{item.text}</p>
      <Repeat count={item.repeat} />
    </div>
  );
}

/**
 * The row of a turn that did not end well. A failed turn is a notice row with what stopped it (from
 * the provider error that the agent text reported, `detail`) and the raw error under Details; an
 * interrupted turn is a quiet line (Steer now interrupts a turn on purpose).
 */
export function TurnAlert({
  status,
  detail,
}: {
  readonly status: 'failed' | 'interrupted';
  readonly detail: string | undefined;
}) {
  if (status === 'interrupted') {
    return (
      <p className="fd-chat-line" data-status={status}>
        {TURN_TEXT.interrupted}
      </p>
    );
  }
  const cause = detail === undefined ? '' : apiCause(detail);
  return (
    <div
      className="fd-notice fd-chat-alert"
      data-tone="error"
      data-place="page"
      data-status={status}
    >
      <Icon of={OctagonAlert} size={14} />
      <div className="fd-notice-text">
        <p>{TURN_TEXT.failed}</p>
        {cause === '' ? null : <p className="fd-chat-alert-cause">{cause}</p>}
        {detail === undefined ? null : (
          <details className="fd-chat-alert-details">
            <summary>
              <Icon of={ChevronRight} size={12} />
              Details
            </summary>
            <pre>{detail}</pre>
          </details>
        )}
      </div>
    </div>
  );
}
