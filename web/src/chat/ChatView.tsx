// The chat view of a session: the primary projection of the canonical events
// (UI-PLAN §2.1). It renders `Item`s from `buildTimeline`; it owns no data.
import { type ReactNode, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import type { ProviderKind, ReviewComment, ReviewSummary, SubagentLog } from '../gen';
import { hasChanges } from '../session/changes';
import { shownStatus, type TaskState } from '../session/tasks';
import type { Item, Subagent } from '../session/timeline';
import {
  ArrowDown,
  ChevronRight,
  FileText,
  GitCompare,
  Globe,
  Icon,
  type LucideIcon,
  MessageSquare,
  Pencil,
  Plug,
  Search,
  SquareTerminal,
  TriangleAlert,
  Workflow,
  Wrench,
} from '../ui/icons';
import { addedText, homeShort, homeShortIn, removedText } from '../ui/format';
import { Loading } from '../ui/Loading';
import { ErrorNotice } from '../ui/page';
import { ArtifactChip, SuggestedChip } from '../artifacts/ArtifactChips';
import { memberName } from '../sharing/member';
import { clock } from '../sharing/people';
import { argText, requestTitle, runLabel, thinkingText, toolName } from './labels';
import { NoticeLine, TurnAlert } from './ChatAlert';
import { type NoticeItem, pairTurnErrors } from './notices';
import { applyChatFilter, compactRuns, type Tool, useChatFilter, type ChatFilter } from './filter';
import { FilterBar } from './FilterBar';
import type { HistoryView } from './history';
import { HistoryBlock } from './HistoryBlock';
import { Markdown } from './Markdown';
import { PathTail } from './PathTail';
import { SubagentWork } from './SubagentWork';
import { WorkflowLine } from './WorkflowLine';
import { isShellTool, ShellCard, type ShellEnv, unfoldShells } from './ShellCard';
import { PromptBubble } from './PromptBubble';
import { hasOpenRequest, pairWaiting, WAITING_LABEL, WAITING_SHORT, type Waiting } from './waiting';
import { ReviewCard } from '../review/ReviewCard';
import type { WorkflowView } from '../session/workflows';

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
  /** B12: the workflows of the session; the card of a Workflow call shows the line of its agents. */
  readonly workflows?: ReadonlyMap<string, WorkflowView> | undefined;
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

/** The icon of a tool row: a Claude question has the question glyph of the board (the Needs you
    rail, Watch, the fleet cards), every other call the icon of its kind. */
const toolIcon = (tool: Tool): LucideIcon =>
  tool.title === 'AskUserQuestion' ? MessageSquare : KIND_ICON[tool.tool_kind];

/**
 * The node of a finished tool call on the flight path (DESIGN-SYSTEM §4.5): its shape and color
 * come from `data-status` (chat.css); a running call shows the working glyph instead.
 */
function StatusMark({ status }: { status: Exclude<Tool['status'], 'running'> | 'waiting' }) {
  return <span className="fd-chat-mark" role="img" aria-label={status} data-status={status} />;
}

/** The result at the right of a finished call: `ok`, or how it failed. */
const RESULT: Partial<Record<Tool['status'], { text: string; tone?: 'hazard' }>> = {
  ok: { text: 'ok' },
  failed: { text: 'failed', tone: 'hazard' },
  denied: { text: 'denied', tone: 'hazard' },
};

const basename = (path: string): string => path.split('/').filter(Boolean).pop() ?? path;

/**
 * The fades of the log (chat.css): away from its top it fades under the tab row
 * (`data-scrolled`); with more below it fades above the composer (`data-more-below`).
 */
function markEdges(el: HTMLElement) {
  el.toggleAttribute('data-scrolled', el.scrollTop > 0);
  el.toggleAttribute('data-more-below', el.scrollHeight - el.scrollTop - el.clientHeight > 1);
}

/**
 * The room that the scrollbar of the log takes at its start and its end (`scrollbar-gutter:
 * stable both-edges` keeps half of it at each side; a phone keeps it at the end only; an overlay
 * scrollbar takes none), as `--chat-sb-start` and `--chat-sb-end` on the chat: the Agents bar
 * above the log keeps the same room, so that its menus start and end on the edges of the prose.
 */
function markGutter(el: HTMLElement, chat: HTMLElement) {
  const room = Math.max(0, el.offsetWidth - el.clientWidth);
  const start = getComputedStyle(el).scrollbarGutter.includes('both-edges') ? room / 2 : 0;
  for (const [name, px] of [
    ['--chat-sb-start', start],
    ['--chat-sb-end', room - start],
  ] as const) {
    if (chat.style.getPropertyValue(name) !== `${px}px`) chat.style.setProperty(name, `${px}px`);
  }
}

type ItemsProps = {
  items: readonly Item[];
  onOpenChanges: OpenChanges;
  compact: boolean;
  provider: ProviderKind | null;
  review?: ChatReview | undefined;
  loadSubagent?: ((toolUseId: string) => Promise<SubagentLog>) | null | undefined;
  tasks?: ReadonlyMap<string, TaskState> | undefined;
  shell?: ShellEnv | undefined;
  workflows?: ReadonlyMap<string, WorkflowView> | undefined;
};

const NO_TASKS: ReadonlyMap<string, TaskState> = new Map();

function Items(props: ItemsProps): ReactNode {
  const { compact, provider, onOpenChanges, review, loadSubagent, tasks, shell } = props;
  // §4.7: a failed turn shows the provider error of its last agent text; the text is not repeated.
  const turnErrors = pairTurnErrors(props.items);
  const items =
    turnErrors.hidden.size === 0
      ? props.items
      : props.items.filter((item) => !turnErrors.hidden.has(item.key));
  // B8: a shell card is never folded.
  const rows = unfoldShells(compactRuns(items, compact, provider), compact, provider);
  // §4.5: a call that waits for an answer shows it in its own row.
  const waiting = pairWaiting(items);
  return rows.map((row) =>
    row.kind === 'fold' ? (
      <ToolFold
        key={row.items[0]?.key}
        items={row.items}
        failed={row.failed}
        summary={row.summary}
        waiting={waiting}
        workflows={props.workflows}
        {...{ onOpenChanges, compact, provider, review, loadSubagent, tasks, shell }}
      />
    ) : (
      <ItemView
        key={row.item.key}
        item={row.item}
        waiting={waiting}
        turnErrors={turnErrors.detail}
        workflows={props.workflows}
        {...{ onOpenChanges, compact, provider, review, loadSubagent, tasks, shell }}
      />
    ),
  );
}

/** DL-175: a run of finished tool rows (and notes) folded into one row. A run with a failure starts open. */
function ToolFold(
  props: Omit<ItemsProps, 'items'> & {
    items: Item[];
    failed: number;
    summary: string;
    waiting: Waiting;
  },
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

/** A tool call. `waitingFor`: the title of the open request of the call (it waits for you). */
function ToolView({ tool, waitingFor }: { tool: Tool; waitingFor?: string | undefined }) {
  // The row shows the input as one line (a JSON object as its values); the body keeps it raw.
  const target = argText(tool.input);
  const { name, server } = toolName(tool.title);
  // One file that the target already names is not repeated as a badge.
  const named =
    tool.files.length === 1 && tool.files[0] !== undefined && target.includes(tool.files[0]);
  const shown = named ? [] : tool.files.slice(0, MAX_FILE_CHIPS);
  const more = named ? 0 : tool.files.length - shown.length;
  // A file target loses its leading folders, not its end: the end names the file (PathTail, with
  // the start ellipsis of chat.css `[data-path]` as the fallback).
  const isPath =
    (tool.files.length === 1 && target === tool.files[0]) ||
    tool.tool_kind === 'read' ||
    tool.tool_kind === 'edit';
  const failedExit = tool.exit_code !== null && tool.exit_code !== 0;
  const result = failedExit ? undefined : RESULT[tool.status];
  return (
    <details
      className="fd-chat-tool"
      data-status={tool.status}
      data-node={tool.node_id}
      data-waiting={waitingFor === undefined ? undefined : ''}
    >
      <summary>
        {waitingFor !== undefined ? (
          <StatusMark status="waiting" />
        ) : tool.status === 'running' ? (
          <span className="fd-dot" data-status="working" role="img" aria-label="running" />
        ) : (
          <StatusMark status={tool.status} />
        )}
        <span className="fd-chat-kind" title={tool.tool_kind}>
          <Icon of={toolIcon(tool)} size={14} />
        </span>
        {/* A name in words (an MCP tool, a built-in tool) keeps the raw id in its tooltip. */}
        <span className="fd-chat-tool-title" title={name === tool.title ? undefined : tool.title}>
          {name}
        </span>
        {server === null ? null : <span className="fd-chat-tool-server">{server}</span>}
        {target !== '' ? (
          // A home folder reads as `~`, the same as in the prose and the session header.
          <span className="fd-chat-tool-arg" data-path={isPath ? '' : undefined}>
            {isPath ? <PathTail path={homeShort(target)} /> : <bdi>{homeShortIn(target)}</bdi>}
          </span>
        ) : null}
        {shown.map((path) => (
          <span key={path} className="fd-badge fd-chat-file" title={path}>
            {/* The name gives way with an ellipsis on a narrow row (chat.css). */}
            <span>{basename(path)}</span>
          </span>
        ))}
        {more > 0 ? <span className="fd-badge">{`+${more}`}</span> : null}
        {failedExit ? (
          <span className="fd-badge" data-tone="hazard">{`exit ${tool.exit_code}`}</span>
        ) : null}
        {waitingFor !== undefined ? (
          // The phone row shows the short label (chat.css); the approval sheet says the rest.
          <span className="fd-chat-tool-result" data-tone="act">
            <span className="fd-vh">{`${WAITING_LABEL} · ${requestTitle(waitingFor)}`}</span>
            <span className="fd-chat-wait-long" aria-hidden="true">
              {WAITING_LABEL}
            </span>
            <span className="fd-chat-wait-short" aria-hidden="true">
              {WAITING_SHORT}
            </span>
          </span>
        ) : result ? (
          <span className="fd-chat-tool-result" data-tone={result.tone}>
            {result.text}
          </span>
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

function SubagentView(props: Omit<ItemsProps, 'items'> & { item: Subagent; waiting?: Waiting }) {
  // The pairs of the outer list stay out: the entries of the subagent pair their own.
  const { item, waiting: _outer, ...rest } = props;
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

function ItemView(
  props: Omit<ItemsProps, 'items'> & {
    item: Item;
    waiting?: Waiting;
    /** The raw provider error of each failed turn of the list, by turn key (`pairTurnErrors`). */
    turnErrors?: ReadonlyMap<string, string>;
  },
) {
  const { item } = props;
  switch (item.type) {
    case 'run': {
      // Backlog B2: a turn the agent or the station started is one muted line, not a bubble.
      // B8: a `!` command turn shows only its shell card.
      if (item.origin === 'shell') return null;
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
              author={name}
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
          <summary>
            <Icon of={ChevronRight} size={12} />
            {thinkingText(item)}
          </summary>
          <div className="fd-chat-reasoning-text">{item.text}</div>
        </details>
      );
    case 'tool':
      return isShellTool(item) ? (
        <ShellCard tool={item} env={props.shell} />
      ) : (
        <>
          <ToolView tool={item} waitingFor={props.waiting?.tools.get(item.key)} />
          <WorkflowLine workflows={props.workflows} nodeId={item.node_id} />
        </>
      );
    case 'subagent':
      return <SubagentView {...props} item={item} />;
    case 'approval':
      // The row of its call says that it waits (ToolView).
      if (props.waiting?.merged.has(item.key)) return null;
      return item.state === 'open' ? (
        <p className="fd-chat-line fd-chat-ask" data-state="open">
          <Icon of={TriangleAlert} size={14} />
          {`${WAITING_LABEL} · ${requestTitle(item.title)}`}
        </p>
      ) : (
        <p className="fd-chat-line" data-state={item.state}>
          {`${requestTitle(item.title)} · ${item.outcome ?? 'expired'}`}
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
          {item.status !== 'done' ? (
            <TurnAlert status={item.status} detail={props.turnErrors?.get(item.key)} />
          ) : null}
          {item.diff && hasChanges(item.diff) ? (
            <div className="fd-chat-diff">
              <Icon of={GitCompare} size={14} />
              <span>
                {item.diff.files_changed} {item.diff.files_changed === 1 ? 'file' : 'files'} changed
              </span>
              {item.diff.insertions > 0 ? (
                <span className="fd-add">{addedText(item.diff.insertions)}</span>
              ) : null}
              {item.diff.deletions > 0 ? (
                <span className="fd-del">{removedText(item.diff.deletions)}</span>
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
      // The raw text of the station and the repeats are optional fields (`NoticeExtra`).
      return <NoticeLine item={item as NoticeItem} />;
  }
}

export function ChatView(props: ChatViewProps) {
  const { items, working, ready, error } = props;
  const provider = props.provider ?? null;
  const chat = useRef<HTMLElement>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const column = useRef<HTMLDivElement>(null);
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

  // The log also changes its height without a scroll (new rows, an opened row, a filter, a
  // resized window): the fades follow it, and the bar above it keeps the room of its scrollbar.
  useEffect(() => {
    const el = scroller.current;
    const body = column.current;
    const root = chat.current;
    if (!el || !body || !root || typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(() => {
      markEdges(el);
      markGutter(el, root);
    });
    observer.observe(el);
    observer.observe(body);
    return () => observer.disconnect();
  }, []);

  const onScroll = () => {
    const el = scroller.current;
    if (!el) return;
    markEdges(el);
    const near = el.scrollHeight - el.scrollTop - el.clientHeight <= NEAR_BOTTOM_PX;
    stick.current = near;
    if (near) setBehind(false);
    if (el.scrollTop <= HISTORY_TOP_PX && history?.view.more && !history.view.loading) {
      loadEarlier();
    }
  };

  const last = shown.at(-1);
  // A session that waits for your answer is not working (§4.5: one state per row), and a running
  // call at the end shows the working glyph already: one working mark for one state.
  const showWorking =
    working &&
    !(last?.type === 'message' && !last.done) &&
    !(last?.type === 'tool' && last.status === 'running') &&
    !hasOpenRequest(items);

  let body: ReactNode;
  // §4.7: the quiet loading line and the error notice of every panel.
  if (!ready) body = <Loading text="Loading the conversation…" />;
  else if (error) body = <ErrorNotice text={error} />;
  else if (items.length === 0) {
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
          workflows={props.workflows}
        />
      );
  }

  const toolbar = props.toolbar !== undefined && props.toolbar !== null;
  const filterBar =
    ready && !error && items.length > 0 ? (
      <FilterBar filter={filter} counts={filtered.counts} onChange={setFilter} />
    ) : null;

  return (
    <section className="fd-chat" aria-label="Conversation" ref={chat}>
      {/* B6: with the agents switcher, the filters share its bar; alone, they sit at the right
          end of the session tab row (chat.css), out of the transcript. */}
      {toolbar ? (
        <div className="fd-chat-bar">
          <div className="fd-chat-toolbar">{props.toolbar}</div>
          {filterBar}
        </div>
      ) : (
        filterBar
      )}
      {/* biome-ignore lint/a11y/noNoninteractiveTabindex: a keyboard user scrolls the log */}
      <div className="fd-chat-scroll" role="log" ref={scroller} onScroll={onScroll} tabIndex={0}>
        <div className="fd-chat-column" ref={column}>
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
              <span className="fd-dot" data-status="working" aria-hidden="true" />
              Working
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
