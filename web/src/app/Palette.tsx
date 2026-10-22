import { useRef, useState } from 'react';
import { stationName } from '../board/fleet';
import { STATUS_LANES } from '../board/fleet';
import type { DeckSession, SessionKey, StationView } from '../gen';
import { useDismiss } from '../ui/dismiss';
import { Icon, Plus } from '../ui/icons';
import { trapTab, useFocusReturn } from './focus';
import { useOverlay } from './overlay';
import { paletteMatches, sessionTitle } from './sessions';

type Props = {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly onOpen: (key: SessionKey) => void;
  readonly onClose: () => void;
  /** R6.3: when no session matches, the row "Start a session" opens the spawn form with the query. */
  readonly onStart?: ((prompt: string) => void) | undefined;
  /** The query at the first render (tests). */
  readonly initialQuery?: string;
};

const statusLabel = (card: DeckSession) =>
  STATUS_LANES.find((lane) => lane.status === card.status)?.label ?? card.status;

/** "Go to session": type a name, pick with the arrow keys, Enter opens it. */
export function Palette({
  sessions,
  stations,
  onOpen,
  onClose,
  onStart,
  initialQuery = '',
}: Props) {
  const [query, setQuery] = useState(initialQuery);
  const [active, setActive] = useState(0);
  const root = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLInputElement>(null);
  useFocusReturn(root, input);
  useOverlay();
  useDismiss(root, true, onClose);
  const matches = paletteMatches(sessions, query);
  const at = Math.min(active, Math.max(0, matches.length - 1));
  const pick = (card: DeckSession) => {
    onClose();
    onOpen({ station_id: card.station_id, session_id: card.summary.session_id });
  };
  const optionId = (i: number) => `fd-palette-option-${i}`;
  const prompt = query.trim();
  const canStart = matches.length === 0 && prompt !== '' && onStart !== undefined;
  const start = () => {
    if (onStart === undefined) return;
    onClose();
    onStart(prompt);
  };
  return (
    <div className="fd-overlay">
      <div
        ref={root}
        className="fd-dialog fd-palette"
        role="dialog"
        aria-modal="true"
        aria-label="Go to session"
        onKeyDown={(e) => trapTab(e, root.current)}
      >
        <input
          ref={input}
          type="search"
          role="combobox"
          aria-label="Session name"
          aria-expanded="true"
          aria-controls="fd-palette-list"
          aria-activedescendant={matches.length === 0 && !canStart ? undefined : optionId(at)}
          placeholder="Go to session…"
          autoComplete="off"
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            setActive(0);
          }}
          onKeyDown={(e) => {
            if (e.key === 'Escape') {
              e.preventDefault();
              onClose();
            } else if (e.key === 'ArrowDown' && matches.length > 0) {
              e.preventDefault();
              setActive((at + 1) % matches.length);
            } else if (e.key === 'ArrowUp' && matches.length > 0) {
              e.preventDefault();
              setActive((at - 1 + matches.length) % matches.length);
            } else if (e.key === 'Enter') {
              e.preventDefault();
              const card = matches[at];
              if (card !== undefined) pick(card);
              else if (canStart) start();
            }
          }}
        />
        <div id="fd-palette-list" role="listbox" aria-label="Matching sessions">
          {matches.map((card, i) => (
            <div
              key={`${card.station_id}/${card.summary.session_id}`}
              id={optionId(i)}
              role="option"
              tabIndex={-1}
              aria-selected={i === at}
              data-status={card.status}
              onMouseMove={() => setActive(i)}
              onClick={() => pick(card)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') pick(card);
              }}
            >
              <span className="fd-dot" data-status={card.status} aria-hidden="true" />
              <span className="fd-palette-title">{sessionTitle(card)}</span>
              <span className="fd-palette-meta">
                {stationName(card.station_id, stations)} · {statusLabel(card)}
              </span>
            </div>
          ))}
          {canStart ? (
            <div
              id={optionId(0)}
              role="option"
              tabIndex={-1}
              aria-selected="true"
              data-action="start"
              onClick={start}
              onKeyDown={(e) => {
                if (e.key === 'Enter') start();
              }}
            >
              <span className="fd-palette-start" aria-hidden="true">
                <Icon of={Plus} size={14} />
              </span>
              <span className="fd-palette-title">Start a session: “{prompt}”</span>
            </div>
          ) : null}
        </div>
        {matches.length === 0 && !canStart ? (
          <p className="fd-empty">No matching sessions.</p>
        ) : null}
        <div className="fd-palette-foot" aria-hidden="true">
          <span>
            <kbd className="fd-kbd">↑</kbd>
            <kbd className="fd-kbd">↓</kbd> Move
          </span>
          <span>
            <kbd className="fd-kbd">↵</kbd> Open
          </span>
          <span>
            <kbd className="fd-kbd">Esc</kbd> Close
          </span>
        </div>
      </div>
    </div>
  );
}
