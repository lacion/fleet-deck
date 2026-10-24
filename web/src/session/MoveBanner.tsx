// FG-9 (design §12.4, §12.4A, §12.8): the line of a move above the chat: the progress of an open
// move, "Arriving from <station>…" on the new session, "Moved to <station>" with "Open" on the
// source, and "Undo move" for a move whose new session could not resume.
import { useState } from 'react';
import { stationName } from '../board/fleet';
import type { DeckSession, StationView } from '../gen';
import type { MoveView } from '../integrations/actions';
import { Icon, X } from '../ui/icons';
import { MoveUndo } from './MoveUndo';
import { moveNotice } from './moveModel';

type Props = {
  readonly session: DeckSession;
  readonly stations: readonly StationView[];
  readonly move: MoveView | null;
  /** Only the owner undoes a move (§12.4A). */
  readonly owner: boolean;
  readonly onChanged: () => void;
};

export function MoveBanner({ session, stations, move, owner, onChanged }: Props) {
  const [dismissed, setDismissed] = useState<string | null>(null);
  const s = session.summary;
  const notice = moveNotice({ station: session.station_id, summary: s, move, stations });
  if (notice === null || notice.key === dismissed) return null;
  const own = (id: string) => stationName(id, stations);
  const action = notice.action;
  const source = move !== null ? own(move.src_station) : own(session.station_id);
  const target =
    move !== null
      ? own(move.dst_station)
      : own(s.moved_to === undefined ? session.station_id : s.moved_to.station_id);
  return (
    <div
      className="fd-sh-strip"
      data-tone={notice.tone}
      role={notice.tone === 'hazard' ? 'alert' : 'status'}
      aria-label="Move"
    >
      <span>{notice.text}</span>
      {action?.kind === 'open' ? (
        <a className="fd-ghost fd-sm" href={action.href}>
          Open
        </a>
      ) : null}
      {action?.kind === 'undo' && owner ? (
        <MoveUndo move={action.move} source={source} target={target} onUndone={onChanged} />
      ) : null}
      {notice.dismissible ? (
        <button
          type="button"
          className="fd-iconbtn fd-sm"
          aria-label="Dismiss"
          onClick={() => setDismissed(notice.key)}
        >
          <Icon of={X} />
        </button>
      ) : null}
    </div>
  );
}
