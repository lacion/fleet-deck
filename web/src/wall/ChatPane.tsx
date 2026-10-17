// R13 P1: the read-only chat of the focus mode. It shows the newest items of the timeline as
// text: no composer, no filters and no control. (`ChatView` has a filter menu and `Markdown` has a
// copy button on a code block, so the wall draws its own lines.)
import type { DisplayEvent, EventEnvelope } from '../gen';
import { buildTimeline, type Item } from '../session/timeline';
import { displayChatLines } from './displayLines';

/** The most items that the pane draws. */
export const CHAT_ITEMS = 120;

type Line = { readonly key: string; readonly kind: string; readonly text: string };

function lineOf(item: Item): Line | null {
  switch (item.type) {
    case 'run':
      return { key: item.key, kind: 'prompt', text: item.text ?? item.input };
    case 'message':
      return item.text.trim() === '' ? null : { key: item.key, kind: 'message', text: item.text };
    case 'tool': {
      const call = item.input === '' ? item.title : `${item.title}: ${item.input}`;
      const mark = item.status === 'failed' || item.status === 'denied' ? ` (${item.status})` : '';
      return { key: item.key, kind: 'tool', text: `${call.split('\n')[0] ?? ''}${mark}` };
    }
    case 'subagent':
      return { key: item.key, kind: 'tool', text: `Subagent: ${item.title}` };
    case 'plan':
      return { key: item.key, kind: 'message', text: item.markdown };
    case 'approval':
      return {
        key: item.key,
        kind: item.state === 'open' ? 'ask' : 'tool',
        text:
          item.state === 'open'
            ? `Waiting for your answer · ${item.title}`
            : `${item.title} · ${item.outcome ?? 'expired'}`,
      };
    case 'notice':
      return { key: item.key, kind: item.tone === 'error' ? 'error' : 'tool', text: item.text };
    case 'artifact':
      return { key: item.key, kind: 'tool', text: `Artifact: ${item.title}` };
    default:
      return null;
  }
}

/** The lines of the chat, oldest first, at most `max`. */
export function chatLines(events: readonly EventEnvelope[], max: number = CHAT_ITEMS): Line[] {
  return buildTimeline(events)
    .items.map(lineOf)
    .filter((l): l is Line => l !== null)
    .slice(-max);
}

export function ChatPane({
  events,
  display,
  ready,
  unavailable,
}: {
  readonly events: readonly EventEnvelope[];
  /** The events of a wall display timeline; they replace `events` when there are some. */
  readonly display?: readonly DisplayEvent[] | undefined;
  readonly ready: boolean;
  readonly unavailable: boolean;
}) {
  const lines =
    display !== undefined && display.length > 0 ? displayChatLines(display) : chatLines(events);
  return (
    <section className="fd-wall-chat" aria-label="Chat">
      {unavailable ? (
        <p className="fd-wall-note">Unavailable</p>
      ) : lines.length === 0 ? (
        <p className="fd-wall-note">{ready ? 'No chat yet.' : 'Loading…'}</p>
      ) : (
        <div className="fd-wall-chatlines">
          {lines.map((l) => (
            <p key={l.key} className="fd-wall-chatline" data-kind={l.kind}>
              {l.text}
            </p>
          ))}
        </div>
      )}
    </section>
  );
}
