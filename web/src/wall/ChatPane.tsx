// R13 P1: the read-only chat of the focus mode. It shows the newest items of the timeline: no
// composer, no filters and no control. The agent text goes through the chat Markdown in its
// read-only form (no copy button); the tool calls run on a flight path (DESIGN-SYSTEM §4.5).
import { useLayoutEffect, useRef, useState } from 'react';
import { Markdown } from '../chat/Markdown';
import { agentLine } from '../chat/notices';
import { pairWaiting } from '../chat/waiting';
import type { DisplayEvent, EventEnvelope } from '../gen';
import { buildTimeline, type Item } from '../session/timeline';
import { toolText } from '../watch/tooltext';
import { atEnd } from './control';
import { displayChatLines } from './displayLines';

/** The words that a screen reader says for the node of a call that is not done (the node shape
 * shows the state only to the eye). */
const STATE_WORD = {
  running: 'Running',
  waiting: 'Waiting for your answer',
  ended: 'Ended',
} as const;

/** The line at the foot of a chat that the owner scrolled back (B17 §3.1.5). */
export const SCROLLED_BACK_TEXT = 'Scrolled back. New lines are below.';

/** The most items that the pane draws. */
export const CHAT_ITEMS = 120;

type Line = {
  readonly key: string;
  readonly kind: string;
  /** The text; for a tool line, the tool name. */
  readonly text: string;
  /** The target of a tool line (a command, a path), drawn in mono. */
  readonly target?: string | undefined;
  /** How a tool line ended when it did not go well: "failed", "denied". */
  readonly mark?: string | undefined;
  /** The node of a tool call that is not done (DESIGN-SYSTEM §4.5): it runs, it waits for your
   * answer, or it ended without a result. A done call has none (a filled ok node). */
  readonly state?: 'running' | 'waiting' | 'ended' | undefined;
};

function lineOf(item: Item, waiting: ReadonlyMap<string, string>): Line | null {
  switch (item.type) {
    case 'run':
      return { key: item.key, kind: 'prompt', text: item.text ?? item.input };
    case 'message':
      return item.text.trim() === '' ? null : { key: item.key, kind: 'message', text: item.text };
    case 'tool': {
      // The words of the call (watch/tooltext): no raw MCP id, no JSON payload.
      const call = toolText(item.title, item.input, { kind: item.tool_kind, files: item.files });
      const mark = item.status === 'failed' || item.status === 'denied' ? item.status : undefined;
      const state =
        item.status === 'running'
          ? waiting.has(item.key)
            ? 'waiting'
            : 'running'
          : item.status === 'ended'
            ? 'ended'
            : undefined;
      return {
        key: item.key,
        kind: 'tool',
        text: call.name,
        target: call.target === '' ? undefined : call.target,
        mark,
        state,
      };
    }
    case 'subagent':
      return { key: item.key, kind: 'tool', text: 'Subagent', target: item.title };
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
      return { key: item.key, kind: 'tool', text: 'Artifact', target: item.title };
    default:
      return null;
  }
}

/** The lines of the chat, oldest first, at most `max`. */
export function chatLines(events: readonly EventEnvelope[], max: number = CHAT_ITEMS): Line[] {
  const { items } = buildTimeline(events);
  // A running call with an open request waits for your answer; the request keeps its own line.
  const waiting = pairWaiting(items).tools;
  return items
    .map((item) => lineOf(item, waiting))
    .filter((l): l is Line => l !== null)
    .slice(-max);
}

function ChatLine({ line }: { readonly line: Line }) {
  if (line.kind === 'message') {
    // A provider error reads as its cause; the provider's own words stay in the title.
    const said = agentLine(line.text);
    return (
      <div
        className="fd-wall-chatline"
        data-kind="message"
        title={said === line.text ? undefined : line.text.trim()}
      >
        <Markdown text={said} readOnly />
      </div>
    );
  }
  if (line.kind === 'tool') {
    return (
      <p
        className="fd-wall-chatline"
        data-kind="tool"
        data-mark={line.mark}
        data-state={line.state}
      >
        <span className="fd-wall-tool-name">{line.text}</span>
        {line.target === undefined ? null : (
          <>
            {' '}
            <span className="fd-wall-tool-target">{line.target}</span>
          </>
        )}
        {line.mark === undefined ? null : (
          <>
            {' '}
            <span className="fd-wall-tool-mark">{line.mark}</span>
          </>
        )}
        {line.state === undefined ? null : (
          <span className="fd-vh"> · {STATE_WORD[line.state]}</span>
        )}
      </p>
    );
  }
  return (
    <p className="fd-wall-chatline" data-kind={line.kind}>
      {line.text}
    </p>
  );
}

export function ChatPane({
  events,
  display,
  ready,
  unavailable,
  initialBack = false,
}: {
  readonly events: readonly EventEnvelope[];
  /** The events of a wall display timeline; they replace `events` when there are some. */
  readonly display?: readonly DisplayEvent[] | undefined;
  readonly ready: boolean;
  readonly unavailable: boolean;
  /** The first state of the scrolled-back line (tests). */
  readonly initialBack?: boolean;
}) {
  // The chat follows its end. A remote scroll moves it back (the section stays `overflow: hidden`;
  // a programmatic scroll works on it): new lines then do not move it, until `bottom`.
  const box = useRef<HTMLElement>(null);
  const follow = useRef(!initialBack);
  const [back, setBack] = useState(initialBack);
  const onScroll = () => {
    const el = box.current;
    if (el === null) return;
    const end = atEnd(el);
    follow.current = end;
    setBack(!end);
  };
  const lines: readonly Line[] =
    display !== undefined && display.length > 0 ? displayChatLines(display) : chatLines(events);
  useLayoutEffect(() => {
    const el = box.current;
    if (el !== null && follow.current) el.scrollTop = el.scrollHeight;
  });
  return (
    <section
      className="fd-wall-chat"
      aria-label="Chat"
      data-wall-scroll="chat"
      ref={box}
      onScroll={onScroll}
    >
      {unavailable ? (
        <p className="fd-wall-note">Unavailable</p>
      ) : lines.length === 0 ? (
        <p className="fd-wall-note">{ready ? 'No chat yet.' : 'Loading…'}</p>
      ) : (
        <div className="fd-wall-chatlines">
          {lines.map((l) => (
            <ChatLine key={l.key} line={l} />
          ))}
        </div>
      )}
      {back ? <p className="fd-wall-note fd-wall-chat-back">{SCROLLED_BACK_TEXT}</p> : null}
    </section>
  );
}
