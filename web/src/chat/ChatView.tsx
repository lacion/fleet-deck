// The chat view of a session: the primary projection of the canonical events
// (UI-PLAN §2.1). It renders `Item`s from `buildTimeline`; it owns no data.
import { type ReactNode, useLayoutEffect, useRef, useState } from 'react';
import { hasChanges } from '../session/changes';
import { type Item, type Subagent, type SubagentStatus, subagentLabel } from '../session/timeline';
import {
  ArrowDown,
  Ban,
  Check,
  ChevronRight,
  FileText,
  GitCompare,
  Globe,
  Icon,
  type LucideIcon,
  Pencil,
  Plug,
  Search,
  SquareTerminal,
  TriangleAlert,
  Workflow,
  Wrench,
  X,
} from '../ui/icons';
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

const KIND_ICON: Record<Tool['tool_kind'], LucideIcon> = {
  shell: SquareTerminal,
  edit: Pencil,
  read: FileText,
  search: Search,
  web: Globe,
  mcp: Plug,
  subagent: Workflow,
  other: Wrench,
};

const firstLine = (text: string): string => text.split('\n')[0] ?? '';

/** The mark of a finished tool call; a running call shows a dot instead. */
function StatusMark({ status }: { status: Exclude<Tool['status'], 'running'> }) {
  return (
    <span className="fd-chat-mark" role="img" aria-label={status} data-status={status}>
      {status === 'ok' ? (
        <Icon of={Check} size={14} />
      ) : status === 'failed' ? (
        <Icon of={X} size={14} />
      ) : status === 'denied' ? (
        <Icon of={Ban} size={14} />
      ) : (
        '–'
      )}
    </span>
  );
}

const subagentTone = (status: SubagentStatus): 'ok' | 'info' | 'hazard' | 'muted' => {
  if (status === 'running') return 'ok';
  if (status === 'background') return 'info';
  if (status === 'failed') return 'hazard';
  return 'muted';
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
      <summary>
        <Icon of={ChevronRight} size={12} />
        {summary}
      </summary>
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
  return (
    <details className="fd-chat-tool" data-status={tool.status}>
      <summary>
        {tool.status === 'running' ? (
          <span className="fd-dot" data-status="working" role="img" aria-label="running" />
        ) : (
          <StatusMark status={tool.status} />
        )}
        <span className="fd-chat-kind" title={tool.tool_kind}>
          <Icon of={KIND_ICON[tool.tool_kind]} size={14} />
        </span>
        <span className="fd-chat-tool-title">{tool.title}</span>
        {tool.input ? <span className="fd-chat-tool-arg">{firstLine(tool.input)}</span> : null}
        {shown.map((path) => (
          <span key={path} className="fd-badge" title={path}>
            {basename(path)}
          </span>
        ))}
        {more > 0 ? <span className="fd-badge">{`+${more}`}</span> : null}
        {tool.exit_code !== null && tool.exit_code !== 0 ? (
          <span className="fd-badge" data-tone="hazard">{`exit ${tool.exit_code}`}</span>
        ) : null}
        <span className="fd-chat-chev">
          <Icon of={ChevronRight} size={14} />
        </span>
      </summary>
      <div className="fd-chat-tool-body">
        {tool.input ? <pre className="fd-code">{tool.input}</pre> : null}
        <pre className="fd-code fd-chat-output">{tool.output || 'No output.'}</pre>
      </div>
    </details>
  );
}

function SubagentView(props: { item: Subagent; onOpenChanges: OpenChanges }) {
  const { item } = props;
  const live = item.status === 'running' || item.status === 'background';
  return (
    <div
      className="fd-chat-card fd-chat-subagent fd-panel"
      data-status={item.status}
      data-subagent={item.node_id}
    >
      <div className="fd-chat-card-head">
        <Icon of={Workflow} size={14} />
        <span className="fd-chat-card-title">{item.title}</span>
        <span className="fd-badge" data-tone={subagentTone(item.status)}>
          {subagentLabel(item.status)}
        </span>
      </div>
      {item.input ? <p className="fd-chat-subagent-task">{item.input}</p> : null}
      <details open={live ? true : undefined}>
        <summary>
          <Icon of={ChevronRight} size={12} />
          {`${item.items.length} ${item.items.length === 1 ? 'entry' : 'entries'}`}
        </summary>
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
          {runLabel(item.origin) !== 'You' ? (
            <span className="fd-chat-run-label">{runLabel(item.origin)}</span>
          ) : null}
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
            <Icon of={ChevronRight} size={12} />
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
        <p className="fd-chat-line fd-chat-ask" data-state="open">
          <Icon of={TriangleAlert} size={14} />
          Waiting for your answer · {item.title}
        </p>
      ) : (
        <p className="fd-chat-line" data-state={item.state}>
          {`${item.title} · ${item.outcome ?? 'expired'}`}
        </p>
      );
    case 'plan':
      return (
        <div className="fd-chat-card fd-chat-plan fd-panel">
          <h3 className="fd-label fd-chat-plan-label">Plan</h3>
          <Markdown text={item.markdown} />
        </div>
      );
    case 'turn':
      if (item.status === 'done' && !hasChanges(item.diff)) return null;
      return (
        <div className="fd-chat-turn">
          {item.status !== 'done' ? <p className="fd-chat-line">{`Turn ${item.status}`}</p> : null}
          {item.diff && hasChanges(item.diff) ? (
            <div className="fd-chat-diff">
              <Icon of={GitCompare} size={14} />
              <span>
                {item.diff.files_changed} {item.diff.files_changed === 1 ? 'file' : 'files'} changed
              </span>
              {item.diff.insertions > 0 ? (
                <span className="fd-add">+{item.diff.insertions}</span>
              ) : null}
              {item.diff.deletions > 0 ? (
                <span className="fd-del">−{item.diff.deletions}</span>
              ) : null}
              {props.onOpenChanges ? (
                <button
                  type="button"
                  className="fd-ghost fd-sm"
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
          className="fd-chat-jump fd-sm"
          onClick={() => {
            toBottom(true);
            stick.current = true;
            setBehind(false);
          }}
        >
          <Icon of={ArrowDown} size={14} /> Jump to latest
        </button>
      ) : null}
    </section>
  );
}
