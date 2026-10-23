// B8: the card of a `!` shell command: the command, its output, how it ended, a Stop button
// while it runs, and "Release the folder" for a command that cannot be proven ended.
import { type ReactNode, useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { trapTab, useFocusReturn } from '../app/focus';
import { useOverlay } from '../app/overlay';
import type { ProviderKind } from '../gen';
import type { Outcome } from '../session/commands';
import type { Item } from '../session/timeline';
import { Icon, SquareTerminal } from '../ui/icons';
import { ErrorNotice } from '../ui/page';
import { type CompactRow, foldSummary } from './filter';

type Tool = Extract<Item, { type: 'tool' }>;

/** What the card needs from the session: the owner's actions and whether the session ended. */
export type ShellEnv = {
  /** The owner may stop the running command (an interrupt of the session). */
  readonly canStop: boolean;
  readonly onStop: () => void;
  /** The owner may release a folder (the `admin` scope). */
  readonly canRelease: boolean;
  /** The session is lost, exited or hibernated. */
  readonly ended: boolean;
  /** Releases the folder of the shell command `effectId`; the outcome carries the station text. */
  readonly onRelease: (effectId: string) => Promise<Outcome>;
};

/** The node id of a Claude shell item is `shell-<effect id>`. */
const NODE_PREFIX = 'shell-';

/** How long a command runs with no new output before the owner may release the folder. */
export const STALE_MS = 30_000;

/** A shell item: a Claude `!` command, or a Codex user shell item. */
export const isShellTool = (tool: Tool): boolean =>
  tool.node_id.startsWith(NODE_PREFIX) ||
  (tool.tool_kind === 'shell' && tool.title === 'Shell command');

/** The effect id of a Claude shell item; `null` for a Codex item. */
export const effectIdOf = (tool: Tool): string | null =>
  tool.node_id.startsWith(NODE_PREFIX) ? tool.node_id.slice(NODE_PREFIX.length) : null;

export type ShellState = {
  readonly text: string;
  readonly tone: 'act' | 'ok' | 'hazard' | 'muted';
};

/** True when the item has no completion yet, or the session ended before it. */
export const unsettled = (tool: Tool): boolean =>
  tool.status === 'running' || (tool.status === 'ended' && tool.shell === undefined);

const ENDED: Readonly<Record<string, ShellState>> = {
  cancelled: { text: 'Cancelled', tone: 'muted' },
  timeout: { text: 'Timed out', tone: 'hazard' },
  unknown: { text: 'Unknown: the station restarted during the command', tone: 'hazard' },
  released: { text: 'Released', tone: 'muted' },
};

/** The state text of a shell item. */
export function shellState(tool: Tool): ShellState {
  if (tool.status === 'running') return { text: 'Running…', tone: 'act' };
  const named = tool.shell === undefined ? undefined : ENDED[tool.shell];
  if (named !== undefined) return named;
  if (tool.status === 'ended') return { text: 'Ended with the session', tone: 'muted' };
  if (tool.exit_code !== null) {
    return { text: `exit ${tool.exit_code}`, tone: tool.exit_code === 0 ? 'ok' : 'hazard' };
  }
  return tool.status === 'ok' ? { text: 'exit 0', tone: 'ok' } : { text: 'Failed', tone: 'hazard' };
}

/**
 * "Release the folder" is offered for a command without a completion, to the owner:
 * when the session is not live (or the timeline ended the card with the session), or when the
 * command runs without new output for a while.
 */
export function releaseOffered(tool: Tool, env: ShellEnv | undefined, stale: boolean): boolean {
  return (
    env?.canRelease === true &&
    effectIdOf(tool) !== null &&
    unsettled(tool) &&
    (env.ended || tool.status === 'ended' || stale)
  );
}

type DialogProps = {
  readonly command: string;
  readonly onRelease: () => Promise<Outcome>;
  readonly onClose: () => void;
};

/** The question before a release. The station text of a refusal shows in the dialog. */
export function ReleaseDialog({ command, onRelease, onClose }: DialogProps) {
  const root = useRef<HTMLDivElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  useFocusReturn(root, cancel);
  useOverlay();
  const [busy, setBusy] = useState(false);
  const [refusal, setRefusal] = useState<string | null>(null);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const release = async () => {
    setBusy(true);
    setRefusal(null);
    const outcome = await onRelease();
    if (!alive.current) return;
    setBusy(false);
    if (outcome.ok) onClose();
    else setRefusal(outcome.text);
  };
  return (
    // The overlay click is a mouse shortcut; the Cancel button and Escape serve the keyboard.
    <div
      className="fd-overlay"
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        ref={root}
        className="fd-dialog"
        role="dialog"
        aria-modal="true"
        aria-label="Release this folder"
        onKeyDown={(event) => {
          if (event.key === 'Escape') {
            event.stopPropagation();
            onClose();
          } else trapTab(event, root.current);
        }}
      >
        <h2>Release this folder?</h2>
        <p>
          Only do this when you are sure that <code>!{command}</code> is no longer running (for
          example after a reboot or after you checked the processes).
        </p>
        {refusal === null ? null : <ErrorNotice text={refusal} />}
        <div className="fd-chat-shell-actions">
          <button
            type="button"
            className="fd-danger"
            disabled={busy}
            onClick={() => void release()}
          >
            {busy ? 'Releasing…' : 'Release'}
          </button>
          <button type="button" ref={cancel} onClick={onClose}>
            Cancel
          </button>
        </div>
      </div>
    </div>
  );
}

type CardProps = {
  readonly tool: Tool;
  readonly env?: ShellEnv | undefined;
  /** The wait before a silent running command may be released (tests give a shorter one). */
  readonly staleMs?: number;
};

/** The card of a shell command. */
export function ShellCard({ tool, env, staleMs = STALE_MS }: CardProps) {
  const running = tool.status === 'running';
  const [stale, setStale] = useState(false);
  const [asking, setAsking] = useState(false);
  // The client clock: 30 s with no change of the output. A reload starts the wait again.
  useEffect(() => {
    setStale(false);
    if (!running) return undefined;
    const timer = setTimeout(() => setStale(true), staleMs);
    return () => clearTimeout(timer);
  }, [running, tool.output, staleMs]);
  const state = shellState(tool);
  const effectId = effectIdOf(tool);
  const release = releaseOffered(tool, env, stale);
  const dialog: ReactNode =
    asking && effectId !== null && env !== undefined
      ? createPortal(
          <ReleaseDialog
            command={tool.input}
            onRelease={() => env.onRelease(effectId)}
            onClose={() => setAsking(false)}
          />,
          document.body,
        )
      : null;
  return (
    <div
      className="fd-chat-card fd-chat-shell fd-panel"
      data-status={tool.status}
      data-node={tool.node_id}
      data-shell={tool.shell}
    >
      <div className="fd-chat-card-head">
        {running ? (
          <span className="fd-dot" data-status="working" role="img" aria-label="running" />
        ) : null}
        <Icon of={SquareTerminal} size={14} />
        <span className="fd-chat-card-title">Shell command</span>
        <span className="fd-badge" data-tone={state.tone}>
          {state.text}
        </span>
      </div>
      <pre className="fd-code fd-chat-shell-cmd">{tool.input}</pre>
      <pre className="fd-code fd-chat-output" aria-label="Output">
        {tool.output || (running ? 'No output yet.' : 'No output.')}
      </pre>
      {(running && env?.canStop === true) || release ? (
        <div className="fd-chat-shell-actions">
          {running && env?.canStop === true ? (
            <button type="button" className="fd-sm" onClick={env.onStop}>
              Stop
            </button>
          ) : null}
          {release ? (
            <button type="button" className="fd-sm" onClick={() => setAsking(true)}>
              Release the folder
            </button>
          ) : null}
        </div>
      ) : null}
      {dialog}
    </div>
  );
}

/**
 * A shell card is never folded: the runs that `compactRuns` folded are split at each shell
 * item, and the parts that stay long enough fold again.
 */
export function unfoldShells(
  rows: readonly CompactRow[],
  compact: boolean,
  provider: ProviderKind | null,
): CompactRow[] {
  const isShell = (item: Item) => item.type === 'tool' && isShellTool(item);
  const min = compact ? 2 : 3;
  const out: CompactRow[] = [];
  for (const row of rows) {
    if (row.kind === 'item' || !row.items.some(isShell)) {
      out.push(row);
      continue;
    }
    let chunk: Item[] = [];
    const flush = () => {
      if (chunk.length >= min && chunk.some((i) => i.type === 'tool')) {
        const failed = chunk.filter((i) => i.type === 'tool' && i.status === 'failed').length;
        out.push({ kind: 'fold', items: chunk, failed, summary: foldSummary(chunk, provider) });
      } else {
        for (const item of chunk) out.push({ kind: 'item', item });
      }
      chunk = [];
    };
    for (const item of row.items) {
      if (isShell(item)) {
        flush();
        out.push({ kind: 'item', item });
      } else chunk.push(item);
    }
    flush();
  }
  return out;
}
