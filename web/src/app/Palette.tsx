import { useEffect, useRef, useState } from 'react';
import { STATUS_LANES, stationName } from '../board/fleet';
import type { DeckSession, SessionKey, StationView } from '../gen';
import { archived } from '../ui/archive';
import { useDismiss } from '../ui/dismiss';
import { ArrowDown, ArrowUp, Icon, Plus } from '../ui/icons';
import { isHibernated } from '../views/hibernate';
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

// A hibernated session is in the Live group: its label says Hibernated, not Ended (R6.6).
const statusLabel = (card: DeckSession) =>
  isHibernated(card)
    ? 'Hibernated'
    : (STATUS_LANES.find((lane) => lane.status === card.status)?.label ?? card.status);

/** The two option groups of the list, each under its 12px label. */
const GROUPS = [
  { key: 'live', label: 'Live' },
  { key: 'ended', label: 'Ended' },
] as const;

/**
 * "Go to session": type a name, pick with the arrow keys, Enter opens it. The live sessions come
 * first (needs you, running, idle), the ended ones under their own label; an archived session
 * shows only for a query that matches it.
 */
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
  const groups = paletteMatches(sessions, query, archived.use());
  const matches = [...groups.live, ...groups.ended];
  const at = Math.min(active, Math.max(0, matches.length - 1));
  // The list scrolls (up to 50 rows): the keys keep the active option in view.
  useEffect(() => {
    root.current?.querySelector(`#fd-palette-option-${at}`)?.scrollIntoView?.({ block: 'nearest' });
  }, [at]);
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
          {GROUPS.map(({ key, label }) => {
            const cards = groups[key];
            if (cards.length === 0) return null;
            // The index of an option in the whole list: the arrow keys walk both groups.
            const from = key === 'live' ? 0 : groups.live.length;
            return (
              <div key={key} role="group" aria-labelledby={`fd-palette-${key}`}>
                <div id={`fd-palette-${key}`} role="presentation" className="fd-label">
                  {label}
                </div>
                {cards.map((card, n) => {
                  const i = from + n;
                  return (
                    <div
                      key={`${card.station_id}/${card.summary.session_id}`}
                      id={optionId(i)}
                      role="option"
                      tabIndex={-1}
                      className="fd-selbar"
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
                  );
                })}
              </div>
            );
          })}
          {canStart ? (
            <div
              id={optionId(0)}
              role="option"
              tabIndex={-1}
              className="fd-selbar"
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
        {/* The arrow keys are icons: Mona Sans has no arrow glyph, and the fallback glyph drew
            about 5px tall and above the center of its chip. Return is a word, as Esc is (the help
            says Ctrl+Enter too). */}
        <div className="fd-palette-foot" aria-hidden="true">
          <span>
            <kbd className="fd-kbd fd-kbd-icon">
              <Icon of={ArrowUp} size={12} />
              <span className="fd-vh">Up</span>
            </kbd>
            <kbd className="fd-kbd fd-kbd-icon">
              <Icon of={ArrowDown} size={12} />
              <span className="fd-vh">Down</span>
            </kbd>{' '}
            Move
          </span>
          <span>
            <kbd className="fd-kbd">Enter</kbd> Open
          </span>
          <span>
            <kbd className="fd-kbd">Esc</kbd> Close
          </span>
        </div>
      </div>
    </div>
  );
}
