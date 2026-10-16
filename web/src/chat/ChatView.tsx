// The chat view of a session: the primary projection of the canonical events
// (UI-PLAN §2.1). It renders `Item`s from `buildTimeline`; it owns no data.
import { type ReactNode, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import type { ProviderKind, ReviewComment, ReviewSummary, SubagentLog } from '../gen';
import { hasChanges } from '../session/changes';
import { shownStatus, type TaskState } from '../session/tasks';
import type { Item, Subagent } from '../session/timeline';
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
import { ArtifactChip, SuggestedChip } from '../artifacts/ArtifactChips';
import { memberName } from '../sharing/member';
import { clock } from '../sharing/people';
import { runLabel } from './labels';
import { applyChatFilter, compactRuns, type Tool, useChatFilter, type ChatFilter } from './filter';
import { FilterBar } from './FilterBar';
import type { HistoryView } from './history';
import { HistoryBlock } from './HistoryBlock';
import { Markdown } from './Markdown';
import { SubagentWork } from './SubagentWork';
import { isShellTool, ShellCard, type ShellEnv, unfoldShells } from './ShellCard';
import { PromptBubble } from './PromptBubble';
import { ReviewCard } from '../review/ReviewCard';

/** The review comments of this session (DL-179): the cards, and the chips of the comment ids. */
export type ChatReview = {
  readonly reviews: readonly ReviewSummary[];
  readonly byShort: ReadonlyMap<string, ReviewComment>;
  /** The short ids that the caller may see. */
  readonly known: ReadonlySet<string>;
  /** A chip or a card row: open the thread in Changes. */
  onOpen(shortId: string): void;
};

export type ChatViewProps = {
  readonly items: readonly Item[];
  readonly working: boolean;
  readonly ready: boolean;
  readonly error: string | null;
  readonly onOpenChanges?: (turnId: string) => void;
  /** The provider of the session; it decides how a tool name is classified. */
  readonly provider?: ProviderKind | null;
  /** The filter to start with; the saved filter of this browser when absent (tests). */
  readonly initialFilter?: ChatFilter;
  readonly review?: ChatReview | undefined;
  /** DL-189: the earlier history of an imported session, above the timeline. */
  readonly history?: { readonly view: HistoryView; readonly gap: boolean } | undefined;
  /** DL-191: the owner's reader of a subagent log; `null` or absent for a member. */
  readonly loadSubagent?: ((toolUseId: string) => Promise<SubagentLog>) | null | undefined;
  /** B6: the tasks of the session; a subagent card shows the status of its task. */
  readonly tasks?: ReadonlyMap<string, TaskState> | undefined;
  /** B6: a slot next to the filters (the agents switcher); it shows alone when there are no items. */
  readonly toolbar?: ReactNode;
  /** B6: scroll to the tool card of this node and mark it for 2 s; a new `nonce` repeats it. */
  readonly highlight?: { readonly node_id: string; readonly nonce: number } | null | undefined;
  /** B6: rendered in the chat column after the items and the working dots. */
  readonly footer?: ReactNode;
  /** B6: the text of an empty list; the main chat keeps its own. */
  readonly emptyText?: string | undefined;
  /** B8: what a shell card does (Stop, Release); absent: the cards only show. */
  readonly shell?: ShellEnv | undefined;
};

type OpenChanges = ((turnId: string) => void) | undefined;

const NEAR_BOTTOM_PX = 80;
const MAX_FILE_CHIPS = 3;
const HISTORY_TOP_PX = 40;
const HIGHLIGHT_MS = 2000;

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

const basename = (path: string): string => path.split('/').filter(Boolean).pop() ?? path;

type ItemsProps = {
  items: readonly Item[];
  onOpenChanges: OpenChanges;
  compact: boolean;
  provider: ProviderKind | null;
  review?: ChatReview | undefined;
  loadSubagent?: ((toolUseId: string) => Promise<SubagentLog>) | null | undefined;
  tasks?: ReadonlyMap<string, TaskState> | undefined;
  shell?: ShellEnv | undefined;
};

const NO_TASKS: ReadonlyMap<string, TaskState> = new Map();

function Items(props: ItemsProps): ReactNode {
  const { compact, provider, onOpenChanges, review, loadSubagent, tasks, shell } = props;
  // B8: a shell card is never folded.
  const rows = unfoldShells(compactRuns(props.items, compact, provider), compact, provider);
  return rows.map((row) =>
    row.kind === 'fold' ? (
      <ToolFold
        key={row.items[0]?.key}
        items={row.items}
        failed={row.failed}
        summary={row.summary}
        {...{ onOpenChanges, compact, provider, review, loadSubagent, tasks, shell }}
      />
    ) : (
      <ItemView
        key={row.item.key}
        item={row.item}
        {...{ onOpenChanges, compact, provider, review, loadSubagent, tasks, shell }}
      />
    ),
  );
}

/** DL-175: a run of finished tool rows (and notes) folded into one row. A run with a failure starts open. */
function ToolFold(
  props: Omit<ItemsProps, 'items'> & { items: Item[]; failed: number; summary: string },
) {
  const { items, failed, summary, ...rest } = props;
  return (
    <details
      className="fd-chat-toolgroup fd-chat-toolfold"
      open={failed > 0 ? true : undefined}
      data-failed={failed > 0 ? '' : undefined}
    >
      <summary>
        <Icon of={ChevronRight} size={12} />
        {summary}
        {failed > 0 ? (
          <span className="fd-badge" data-tone="hazard">{`${failed} failed`}</span>
        ) : null}
      </summary>
      <div className="fd-chat-toolgroup-body">
        {items.map((item) => (
          <ItemView key={item.key} item={item} {...rest} />
        ))}
      </div>
    </details>
  );
}

function ToolView({ tool }: { tool: Tool }) {
  const shown = tool.files.slice(0, MAX_FILE_CHIPS);
  const more = tool.files.length - shown.length;
  return (
    <details className="fd-chat-tool" data-status={tool.status} data-node={tool.node_id}>
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

function SubagentView(props: Omit<ItemsProps, 'items'> & { item: Subagent }) {
  const { item, ...rest } = props;
  const { loadSubagent } = rest;
  const toolUseId = item.tool_use_id;
  const shown = shownStatus(item, rest.tasks ?? NO_TASKS);
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
        <span className="fd-badge" data-tone={shown.tone}>
          {shown.label}
        </span>
      </div>
      {item.input ? <p className="fd-chat-subagent-task">{item.input}</p> : null}
      <details open={live ? true : undefined}>
        <summary>
          <Icon of={ChevronRight} size={12} />
          {`${item.items.length} ${item.items.length === 1 ? 'entry' : 'entries'}`}
        </summary>
        <div className="fd-chat-nested">
          <Items items={item.items} {...rest} />
        </div>
      </details>
      {loadSubagent && toolUseId !== null ? (
        <SubagentWork load={() => loadSubagent(toolUseId)} />
      ) : null}
    </div>
  );
}

function ItemView(props: Omit<ItemsProps, 'items'> & { item: Item }) {
  const { item } = props;
  switch (item.type) {
    case 'run': {
      // Backlog B2: a turn the agent or the station started is one muted line, not a bubble.
      if (item.origin === 'provider')
        return (
          <p className="fd-chat-line" data-origin="provider">
            The agent started a turn on its own.
          </p>
        );
      if (item.origin === 'continuation')
        return (
          <p className="fd-chat-line" data-origin="continuation">
            The station continued the session after a restart.
          </p>
        );
      // R11.5: a prompt of a member shows its name after the time; the owner shows nothing extra.
      const name = item.author === undefined ? null : memberName(item.author);
      // DL-179: a run is a Review card only when its input is the follow-up of a review.
      const sent =
        props.review !== undefined && item.input_id !== undefined
          ? props.review.reviews.find((r) => r.input_id === item.input_id)
          : undefined;
      return (
        <div className="fd-chat-run">
          {runLabel(item.origin) !== 'You' ? (
            <span className="fd-chat-run-label">{runLabel(item.origin)}</span>
          ) : null}
          {sent !== undefined && props.review !== undefined ? (
            <ReviewCard
              review={sent}
              text={item.text ?? item.input}
              byShort={props.review.byShort}
              onOpen={props.review.onOpen}
            />
          ) : (
            <PromptBubble
              text={item.text ?? item.input}
              known={props.review?.known}
              onOpen={props.review?.onOpen}
            />
          )}
          {name === null ? null : (
            <span className="fd-chat-run-label fd-chat-author">
              {item.at === undefined ? '' : `${clock(item.at)} `}— {name}
            </span>
          )}
        </div>
      );
    }
    case 'message':
      return (
        <div className={item.done ? 'fd-chat-message' : 'fd-chat-message fd-chat-streaming'}>
          <Markdown
            text={item.text}
            {...(props.review === undefined
              ? {}
              : { comments: props.review.known, onComment: props.review.onOpen })}
          />
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
      return isShellTool(item) ? (
        <ShellCard tool={item} env={props.shell} />
      ) : (
        <ToolView tool={item} />
      );
    case 'subagent':
      return <SubagentView {...props} item={item} />;
    case 'approval':
      return item.state === 'open' ? (
        <p className="fd-chat-line fd-chat-ask" data-state="open">
          <Icon of={TriangleAlert} size={14} />
          Waiting for your answer · {item.title}
        </p>
      ) : (
        <p className="fd-chat-line" data-state={item.state}>
          {`${item.title} · ${item.outcome ?? 'expired'}`}
          {item.answered_by !== undefined && memberName(item.answered_by) !== null
            ? ` · answered by ${memberName(item.answered_by)}`
            : ''}
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
    case 'artifact':
      return <ArtifactChip chip={item} fromEvent />;
    case 'suggestion':
      return <SuggestedChip path={item.path} />;
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
  const provider = props.provider ?? null;
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true); // the user is near the bottom
  const [behind, setBehind] = useState(false);
  const [filter, setFilter] = useChatFilter(props.initialFilter);
  // The filters pick what renders; the stored timeline (`items`) does not change.
  const filtered = useMemo(
    () => applyChatFilter(items, filter, provider),
    [items, filter, provider],
  );
  const shown = filtered.items;

  const toBottom = (smooth: boolean) => {
    const el = scroller.current;
    if (!el) return;
    const reduced = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
    el.scrollTo({ top: el.scrollHeight, behavior: smooth && !reduced ? 'smooth' : 'auto' });
  };

  const seen = useRef<readonly Item[] | null>(null);
  // DL-189: the scroll position before an older page loads; it keeps the same rows in view.
  const anchor = useRef<{ readonly height: number; readonly top: number } | null>(null);
  const history = props.history;
  const historyCount = history?.view.items.length ?? 0;
  const loadEarlier = () => {
    const el = scroller.current;
    if (el) anchor.current = { height: el.scrollHeight, top: el.scrollTop };
    history?.view.loadEarlier();
  };

  // B6: a chosen background command: open the folds around its card, scroll to it, mark it for 2 s.
  const highlight = props.highlight ?? null;
  useEffect(() => {
    const root = scroller.current;
    if (highlight === null || !root) return;
    const card = root.querySelector<HTMLElement>(`[data-node="${CSS.escape(highlight.node_id)}"]`);
    if (!card) {
      // The Commands filter hides the card: show commands; the effect runs again with the card.
      if (!filter.show.commands) setFilter({ ...filter, show: { ...filter.show, commands: true } });
      return;
    }
    for (
      let up = card.parentElement?.closest('details');
      up;
      up = up.parentElement?.closest('details')
    ) {
      up.open = true;
    }
    card.scrollIntoView({ block: 'center' });
    card.setAttribute('data-highlight', '');
    const timer = setTimeout(() => card.removeAttribute('data-highlight'), HIGHLIGHT_MS);
    return () => {
      clearTimeout(timer);
      card.removeAttribute('data-highlight');
    };
  }, [highlight?.node_id, highlight?.nonce, filter.show.commands]);

  useLayoutEffect(() => {
    const el = scroller.current;
    const kept = anchor.current;
    if (kept !== null && el) {
      el.scrollTop = el.scrollHeight - kept.height + kept.top;
      anchor.current = null;
    } else if (stick.current) toBottom(false);
  }, [historyCount]);

  // `stick` is the position before this commit: only a scroll by the user changes it.
  // A change of the filter keeps the bottom when it was at the bottom, and is not new output.
  useLayoutEffect(() => {
    const fresh = seen.current !== items;
    const mounted = seen.current === null;
    seen.current = items;
    if (mounted || stick.current) toBottom(false);
    else if (fresh) setBehind(true);
  }, [items, shown, working]);

  const onScroll = () => {
    const el = scroller.current;
    if (!el) return;
    const near = el.scrollHeight - el.scrollTop - el.clientHeight <= NEAR_BOTTOM_PX;
    stick.current = near;
    if (near) setBehind(false);
    if (el.scrollTop <= HISTORY_TOP_PX && history?.view.more && !history.view.loading) {
      loadEarlier();
    }
  };

  const last = shown.at(-1);
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
    // DL-189: with earlier history above, the empty text would be wrong.
    body =
      historyCount > 0 ? null : (
        <p className="fd-chat-state">
          {props.emptyText ?? 'No messages yet. Send a prompt to start.'}
        </p>
      );
  } else {
    body =
      shown.length === 0 ? (
        <p className="fd-chat-state">Every row is hidden by the filters.</p>
      ) : (
        <Items
          items={shown}
          onOpenChanges={props.onOpenChanges}
          compact={filter.compact}
          provider={provider}
          review={props.review}
          loadSubagent={props.loadSubagent}
          tasks={props.tasks}
          shell={props.shell}
        />
      );
  }

  const filterBar =
    ready && !error && items.length > 0 ? (
      <FilterBar filter={filter} counts={filtered.counts} onChange={setFilter} />
    ) : null;

  return (
    <section className="fd-chat" aria-label="Conversation">
      {props.toolbar === undefined || props.toolbar === null ? (
        filterBar
      ) : (
        <div className="fd-chat-bar">
          <div className="fd-chat-toolbar">{props.toolbar}</div>
          {filterBar}
        </div>
      )}
      {/* biome-ignore lint/a11y/noNoninteractiveTabindex: a keyboard user scrolls the log */}
      <div className="fd-chat-scroll" role="log" ref={scroller} onScroll={onScroll} tabIndex={0}>
        <div className="fd-chat-column">
          {history === undefined ? null : (
            <>
              {history.view.more ? (
                <button
                  type="button"
                  className="fd-ghost fd-sm fd-history-more"
                  disabled={history.view.loading}
                  onClick={loadEarlier}
                >
                  {history.view.loading ? 'Loading…' : 'Load earlier messages'}
                </button>
              ) : null}
              <HistoryBlock view={history.view} gap={history.gap} />
            </>
          )}
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
          {props.footer}
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
