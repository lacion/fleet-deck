// The chat view of a session: the primary projection of the canonical events
// (UI-PLAN §2.1). It renders `Item`s from `buildTimeline`; it owns no data.
import { type ReactNode, useLayoutEffect, useRef, useState } from 'react';
import { type Item, type Subagent, subagentLabel } from '../session/timeline';
import { runLabel } from './labels';
import { Markdown } from './Markdown';

export type ChatViewProps = {
  readonly items: readonly Item[];
  readonly working: boolean;
  readonly ready: boolean;
  readonly error: string | null;
  readonly onOpenChanges?: (turnId: string) => void;
};

type Tool = Extract<Item, { type: 'tool' }>;
type OpenChanges = ((turnId: string) => void) | undefined;

const NEAR_BOTTOM_PX = 80;
const MAX_FILE_CHIPS = 3;

const STATUS_MARK: Record<Tool['status'], string> = {
  running: '',
  ok: '✓',
  failed: '✗',
  denied: '⊘',
  ended: '–',
};

const basename = (path: string): string => path.split('/').filter(Boolean).pop() ?? path;

type Row = { kind: 'item'; item: Item } | { kind: 'tools'; tools: Tool[] };

/** Three or more consecutive tool items become one group. */
function rows(items: readonly Item[]): Row[] {
  const out: Row[] = [];
  let run: Tool[] = [];
  const flush = () => {
    if (run.length >= 3) out.push({ kind: 'tools', tools: run });
    else for (const tool of run) out.push({ kind: 'item', item: tool });
    run = [];
  };
  for (const item of items) {
    if (item.type === 'tool') {
      run.push(item);
    } else {
      flush();
      out.push({ kind: 'item', item });
    }
  }
  flush();
  return out;
}

function Items(props: { items: readonly Item[]; onOpenChanges: OpenChanges }): ReactNode {
  return rows(props.items).map((row) =>
    row.kind === 'tools' ? (
      <ToolGroup key={row.tools[0]?.key} tools={row.tools} />
    ) : (
      <ItemView key={row.item.key} item={row.item} onOpenChanges={props.onOpenChanges} />
    ),
  );
}

function ToolGroup({ tools }: { tools: Tool[] }) {
  const running = tools.filter((t) => t.status === 'running').length;
  const failed = tools.filter((t) => t.status === 'failed').length;
  const summary = `${tools.length} tool calls${running > 0 ? ` · ${running} running` : ''}${
    failed > 0 ? ` · ${failed} failed` : ''
  }`;
  return (
    <details className="fd-chat-toolgroup" open={running > 0 ? true : undefined}>
      <summary>{summary}</summary>
      <div className="fd-chat-toolgroup-body">
        {tools.map((tool) => (
          <ToolView key={tool.key} tool={tool} />
        ))}
      </div>
    </details>
  );
}

function ToolView({ tool }: { tool: Tool }) {
  const shown = tool.files.slice(0, MAX_FILE_CHIPS);
  const more = tool.files.length - shown.length;
  const mark = STATUS_MARK[tool.status];
  return (
    <details className="fd-chat-tool" data-status={tool.status}>
      <summary>
        {tool.status === 'running' ? (
          <span className="fd-chat-dot" role="img" aria-label="running" />
        ) : (
          <span className="fd-chat-mark" role="img" aria-label={tool.status}>
            {mark}
          </span>
        )}
        <span className="fd-chat-tool-title">{tool.title}</span>
        <span className="fd-chat-chip">{tool.tool_kind}</span>
        {shown.map((path) => (
          <span key={path} className="fd-chat-chip fd-chat-file" title={path}>
            {basename(path)}
          </span>
        ))}
        {more > 0 ? <span className="fd-chat-chip">{`+${more}`}</span> : null}
        {tool.exit_code !== null && tool.exit_code !== 0 ? (
          <span className="fd-chat-chip fd-chat-exit">{`exit ${tool.exit_code}`}</span>
        ) : null}
      </summary>
      <div className="fd-chat-tool-body">
        {tool.input ? <pre className="fd-chat-pre">{tool.input}</pre> : null}
        <pre className="fd-chat-pre fd-chat-output">{tool.output || 'No output.'}</pre>
      </div>
    </details>
  );
}

function SubagentView(props: { item: Subagent; onOpenChanges: OpenChanges }) {
  const { item } = props;
  const live = item.status === 'running' || item.status === 'background';
  return (
    <div className="fd-chat-card fd-chat-subagent" data-status={item.status}>
      <div className="fd-chat-card-head">
        <span className="fd-chat-chip">{subagentLabel(item.status)}</span>
        <span className="fd-chat-card-title">{item.title}</span>
      </div>
      <details open={live ? true : undefined}>
        <summary>{`${item.items.length} ${item.items.length === 1 ? 'entry' : 'entries'}`}</summary>
        <div className="fd-chat-nested">
          <Items items={item.items} onOpenChanges={props.onOpenChanges} />
        </div>
      </details>
    </div>
  );
}

function ItemView(props: { item: Item; onOpenChanges: OpenChanges }) {
  const { item } = props;
  switch (item.type) {
    case 'run':
      return (
        <div className="fd-chat-run">
          <span className="fd-chat-run-label">{runLabel(item.origin)}</span>
          <div className="fd-chat-bubble">{item.input}</div>
        </div>
      );
    case 'message':
      return (
        <div className={item.done ? 'fd-chat-message' : 'fd-chat-message fd-chat-streaming'}>
          <Markdown text={item.text} />
        </div>
      );
    case 'reasoning':
      return (
        <details className="fd-chat-reasoning">
          <summary className={item.done ? undefined : 'fd-chat-pulse'}>
            {item.done ? 'Thinking' : 'Thinking…'}
          </summary>
          <div className="fd-chat-reasoning-text">{item.text}</div>
        </details>
      );
    case 'tool':
      return <ToolView tool={item} />;
    case 'subagent':
      return <SubagentView item={item} onOpenChanges={props.onOpenChanges} />;
    case 'approval':
      return item.state === 'open' ? (
        <div className="fd-chat-card fd-chat-approval" data-state="open">
          <div className="fd-chat-card-title">{item.title}</div>
          {item.details ? <pre className="fd-chat-pre">{item.details}</pre> : null}
          <p className="fd-chat-waiting">Waiting for your answer.</p>
        </div>
      ) : (
        <p className="fd-chat-line" data-state={item.state}>
          {`${item.title} · ${item.outcome ?? 'expired'}`}
        </p>
      );
    case 'plan':
      return (
        <div className="fd-chat-card fd-chat-plan">
          <h3 className="fd-chat-card-heading">Plan</h3>
          <Markdown text={item.markdown} />
        </div>
      );
    case 'turn':
      if (item.status === 'done' && item.diff === null) return null;
      return (
        <div className="fd-chat-turn">
          {item.status !== 'done' ? <p className="fd-chat-line">{`Turn ${item.status}`}</p> : null}
          {item.diff ? (
            <div className="fd-chat-diff">
              <span>{`${item.diff.files_changed} files changed · +${item.diff.insertions} −${item.diff.deletions}`}</span>
              {props.onOpenChanges ? (
                <button
                  type="button"
                  className="fd-chat-btn"
                  onClick={() => props.onOpenChanges?.(item.turn_id)}
                >
                  Review changes
                </button>
              ) : null}
            </div>
          ) : null}
        </div>
      );
    case 'notice':
      return (
        <p className={item.tone === 'error' ? 'fd-chat-line fd-chat-error' : 'fd-chat-line'}>
          {item.text}
        </p>
      );
  }
}

export function ChatView(props: ChatViewProps) {
  const { items, working, ready, error } = props;
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true); // the user is near the bottom
  const [behind, setBehind] = useState(false);

  const toBottom = (smooth: boolean) => {
    const el = scroller.current;
    if (!el) return;
    const reduced = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
    el.scrollTo({ top: el.scrollHeight, behavior: smooth && !reduced ? 'smooth' : 'auto' });
  };

  const seen = useRef<readonly Item[] | null>(null);

  // `stick` is the position before this commit: only a scroll by the user changes it.
  useLayoutEffect(() => {
    const fresh = seen.current !== items;
    const mounted = seen.current === null;
    seen.current = items;
    if (mounted || stick.current) toBottom(false);
    else if (fresh) setBehind(true);
  }, [items, working]);

  const onScroll = () => {
    const el = scroller.current;
    if (!el) return;
    const near = el.scrollHeight - el.scrollTop - el.clientHeight <= NEAR_BOTTOM_PX;
    stick.current = near;
    if (near) setBehind(false);
  };

  const last = items.at(-1);
  const showWorking = working && !(last?.type === 'message' && !last.done);

  let body: ReactNode;
  if (!ready) body = <p className="fd-chat-state">Loading the conversation…</p>;
  else if (error) {
    body = (
      <p className="fd-chat-state fd-chat-error" role="alert">
        {error}
      </p>
    );
  } else if (items.length === 0) {
    body = <p className="fd-chat-state">No messages yet. Send a prompt to start.</p>;
  } else {
    body = <Items items={items} onOpenChanges={props.onOpenChanges} />;
  }

  return (
    <section className="fd-chat" aria-label="Conversation">
      {/* biome-ignore lint/a11y/noNoninteractiveTabindex: a keyboard user scrolls the log */}
      <div className="fd-chat-scroll" role="log" ref={scroller} onScroll={onScroll} tabIndex={0}>
        <div className="fd-chat-column">
          {body}
          {ready && !error && showWorking ? (
            <p className="fd-chat-working">
              Working
              <span className="fd-chat-dots" aria-hidden="true">
                <span />
                <span />
                <span />
              </span>
            </p>
          ) : null}
        </div>
      </div>
      {behind ? (
        <button
          type="button"
          className="fd-chat-jump"
          onClick={() => {
            toBottom(true);
            stick.current = true;
            setBehind(false);
          }}
        >
          Jump to latest
        </button>
      ) : null}
    </section>
  );
}
