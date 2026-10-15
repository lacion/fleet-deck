// DL-191: the own messages and tool calls of one subagent, loaded when the owner asks.
import { useEffect, useRef, useState } from 'react';
import type { SubagentLog } from '../gen';
import { ApiError } from '../runtime/api';
import { historyErrorLine, historyStateLine } from './history';
import { HistoryItemView } from './HistoryBlock';

export type SubagentState = {
  readonly log: SubagentLog | null;
  readonly loading: boolean;
  readonly error: string | null;
};

const IDLE: SubagentState = { log: null, loading: false, error: null };

/** The history wording only for the codes that have their own text; else the message itself. */
const HISTORY_CODES: readonly string[] = ['history_unsupported', 'stale_cursor', 'busy'];

function errorText(e: unknown): string {
  if (e instanceof ApiError && e.code !== null && HISTORY_CODES.includes(e.code)) {
    const line = historyErrorLine(e);
    if (line !== null) return line.text;
  }
  return e instanceof Error ? e.message : 'The request failed.';
}

/** "Show its work" under a subagent card. An answer that arrives after a newer request or an unmount is dropped. */
export function SubagentWork({ load }: { readonly load: () => Promise<SubagentLog> }) {
  const [state, setState] = useState<SubagentState>(IDLE);
  const token = useRef(0);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const run = () => {
    const mine = ++token.current;
    setState((s) => ({ log: s.log, loading: true, error: null }));
    load().then(
      (log) => {
        if (alive.current && token.current === mine) setState({ log, loading: false, error: null });
      },
      (e: unknown) => {
        if (alive.current && token.current === mine) {
          setState({ log: null, loading: false, error: errorText(e) });
        }
      },
    );
  };
  return <SubagentWorkView state={state} onRun={run} />;
}

/** The card section for one state: the log, its lines, and the one button. */
export function SubagentWorkView({
  state,
  onRun,
}: {
  readonly state: SubagentState;
  readonly onRun: () => void;
}) {
  const { log, loading, error } = state;
  const line = log === null ? null : historyStateLine(log.state);
  return (
    <div className="fd-subagent-section">
      {log !== null ? (
        log.items.length > 0 ? (
          <div className="fd-subagent-work" role="log" aria-label="Subagent work">
            {log.items.map((item) => (
              <HistoryItemView key={item.id} item={item} />
            ))}
          </div>
        ) : log.state.kind === 'empty' ? (
          <p className="fd-note">The subagent wrote nothing yet.</p>
        ) : null
      ) : null}
      {line === null ? null : <p className="fd-note">{line.text}</p>}
      {error === null ? null : (
        <p className="fd-note fd-subagent-error" role="alert">
          {error}
        </p>
      )}
      <button type="button" className="fd-ghost fd-sm" disabled={loading} onClick={onRun}>
        {loading ? 'Loading…' : log === null ? 'Show its work' : 'Refresh'}
      </button>
    </div>
  );
}
