// FG-9 (design §12.8): "Move to station…" of the session menu: the station select, the checklist
// of §12.2, the kind line and the buttons; and `useMove`, the read of the newest move of a session.
import { useEffect, useRef, useState } from 'react';
import type { DeckSession } from '../gen';
import type { MoveRead } from '../integrations/actions';
import { useIntegrations } from '../integrations/context';
import { Dialog } from '../integrations/Dialog';
import { message, useLoad } from '../integrations/useLoad';
import { Icon, X } from '../ui/icons';
import { checklist, isOpenMove, kindLine, MOVE_POLL_MS, type Mark } from './moveModel';

/** The signature of the move fields of the summary that makes the read run again. */
const moveSignature = (s: DeckSession['summary']): string =>
  `${s.moved_to?.move_id ?? ''}/${s.move_in?.move_id ?? ''}/${s.move_in?.state ?? ''}`;

/**
 * The newest move of the session (owner only). It reads when the session opens, when `moved_to`
 * or `move_in` changes, and every 2 s while the move is open. A failed read keeps the last answer
 * and tries again at the next tick.
 */
export function useMove(
  session: DeckSession,
  enabled: boolean,
): { readonly read: MoveRead | null; readonly reload: () => void } {
  const actions = useIntegrations()?.actions;
  const station = session.station_id;
  const id = session.summary.session_id;
  const { state, reload } = useLoad<MoveRead | null>(
    async () => (enabled && actions !== undefined ? actions.moveRead(station, id) : null),
    `${station}|${id}|${enabled}|${moveSignature(session.summary)}`,
  );
  const last = useRef<MoveRead | null>(null);
  if (state.kind === 'ok') last.current = state.value;
  if (!enabled) last.current = null;
  const read = last.current;
  const open = enabled && isOpenMove(read?.move ?? null);
  useEffect(() => {
    if (!open) return undefined;
    const timer = setInterval(reload, MOVE_POLL_MS);
    return () => clearInterval(timer);
  }, [open, reload]);
  return { read, reload };
}

const MARKS: Readonly<Record<Mark, { readonly glyph: string; readonly label: string }>> = {
  passed: { glyph: '✓', label: 'Passed' },
  failed: { glyph: '✕', label: 'Failed' },
  neutral: { glyph: '–', label: 'Not checked' },
};

type Props = {
  readonly session: DeckSession;
  readonly onClose: () => void;
  /** The move started: the session view reads the move again. */
  readonly onStarted: () => void;
  /** The read at the first render (tests). */
  readonly initial?: MoveRead;
};

export function MoveDialog({ session, onClose, onStarted, initial }: Props) {
  const actions = useIntegrations()?.actions;
  const s = session.summary;
  const station = session.station_id;
  const { state } = useLoad<MoveRead>(
    async () => {
      if (actions === undefined) throw new Error('The deck is not available.');
      return actions.moveRead(station, s.session_id);
    },
    `${station}|${s.session_id}`,
    initial,
  );
  const targets = state.kind === 'ok' ? state.value.targets : [];
  const [chosen, setChosen] = useState<string | null>(null);
  // Until the person chooses, the first ready station is selected.
  const pick = chosen ?? targets.find((t) => t.ready)?.station_id ?? '';
  const target = targets.find((t) => t.station_id === pick);
  const list = checklist(target);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const move = async () => {
    if (actions === undefined || target === undefined || !target.ready) return;
    setBusy(true);
    setError(null);
    try {
      await actions.moveStart(station, s.session_id, target.station_id);
      onStarted();
      onClose();
    } catch (e) {
      setError(message(e));
      setBusy(false);
    }
  };
  return (
    <Dialog title={`Move ${s.callsign ?? s.session_id} to another station`} onClose={onClose}>
      <form
        className="fd-int-form"
        onSubmit={(e) => {
          e.preventDefault();
          void move();
        }}
      >
        {state.kind === 'loading' ? <p className="fd-note">Reading the stations…</p> : null}
        {state.kind === 'error' ? (
          <p className="fd-note fd-int-error" role="alert">
            {state.text}
          </p>
        ) : null}
        {state.kind === 'ok' && targets.length === 0 ? (
          <p className="fd-note">No other station has a home of this project.</p>
        ) : null}
        {targets.length > 0 ? (
          <>
            <label>
              Station
              <select value={pick} onChange={(e) => setChosen(e.target.value)}>
                <option value="">Choose a station</option>
                {targets.map((t) => (
                  <option key={t.station_id} value={t.station_id}>
                    {t.name}
                  </option>
                ))}
              </select>
            </label>
            <ul className="fd-int-list" aria-label="Conditions of a move">
              {list.items.map((item) => (
                <li key={item.label} data-mark={item.mark}>
                  <span role="img" aria-label={MARKS[item.mark].label}>
                    {MARKS[item.mark].glyph}
                  </span>{' '}
                  {item.label}
                  {item.text === undefined ? null : `: ${item.text}`}
                </li>
              ))}
            </ul>
            {list.note === null ? null : <p className="fd-note">{list.note}</p>}
            {target === undefined ? null : <p className="fd-note">{kindLine(target.kind)}</p>}
          </>
        ) : null}
        {error === null ? null : (
          <p className="fd-note fd-int-error" role="alert">
            {error}
          </p>
        )}
        <div className="fd-pane-row">
          <button
            type="submit"
            className="fd-primary"
            disabled={busy || target === undefined || !target.ready}
          >
            Move
          </button>
          <button type="button" className="fd-ghost" disabled={busy} onClick={onClose}>
            <Icon of={X} size={14} /> Cancel
          </button>
        </div>
      </form>
    </Dialog>
  );
}
