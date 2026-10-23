// R13 P1: the race mode: the compare view of one `Nx` batch (DL-170), up to four columns, read-only.
// It has no "Keep this one", no link and no button.
import { sessionTitle } from '../app/sessions';
import { stationName } from '../board/fleet';
import { API_PRICES, costText } from '../board/usage';
import { agentLine } from '../chat/notices';
import { digestOf, lastAgentMessage, patchFiles, siblings } from '../compare/compare';
import type { DeckSession, SessionKey, StationView } from '../gen';
import type { Timeline } from '../runtime/state';
import { hasChanges } from '../session/changes';
import type { SessionActions } from '../session/commands';
import { shortModel } from '../ui/format';
import { refOf } from '../watch/tiles';
import { ChangeFiles, type DisplayEnv, useDisplayChanges } from './displayChanges';
import { lastDisplayMessage } from './displayLines';
import { keyOf, timelineOf, useSessionDiff, useTimelines } from './hooks';

/** The most columns. */
export const RACE_COLUMNS = 4;
/** The most changed files that a column lists. */
const MAX_FILES = 8;

const STATUS_LABEL: Record<DeckSession['status'], string> = {
  queued: 'Queued',
  working: 'Working',
  verifying: 'Verifying',
  needs_you: 'Needs you',
  idle: 'Waiting for a prompt',
  ended: 'Ended',
  unreachable: 'Station unreachable',
  lost: 'Lost',
};

function Column({
  card,
  stations,
  timeline,
  actions,
}: {
  readonly card: DeckSession;
  readonly stations: readonly StationView[];
  readonly timeline: Timeline | undefined;
  readonly actions: SessionActions | undefined;
}) {
  const s = card.summary;
  const state = useSessionDiff(actions?.files ?? null, refOf(card));
  const message = timeline?.ready === true ? lastAgentMessage(timeline.events) : null;
  const digest = digestOf(s);
  const cost = costText(s.usage);
  return (
    <li
      className="fd-wall-col"
      data-session={s.session_id}
      data-status={card.status}
      data-ref={refOf(card)}
    >
      <h2 className="fd-wall-h">{sessionTitle(card)}</h2>
      <p className="fd-wall-note">
        <span className="fd-dot" data-status={card.status} aria-hidden="true" />{' '}
        {STATUS_LABEL[card.status]} · {shortModel(s.model)} ·{' '}
        {stationName(card.station_id, stations)}
        {s.batch === undefined ? '' : ` · ${s.batch.index} of ${s.batch.size}`}
      </p>
      {digest === null ? null : <p className="fd-wall-digest">{digest}</p>}
      <h3 className="fd-wall-label">Changes</h3>
      {state.kind === 'loading' ? <p className="fd-wall-note">Loading the diff…</p> : null}
      {state.kind === 'error' ? <p className="fd-wall-note">{state.text}</p> : null}
      {state.kind === 'ready' ? (
        !hasChanges(state.diff.summary) && state.diff.patch.trim() === '' ? (
          <p className="fd-wall-note">No file changes since the session started.</p>
        ) : (
          <>
            <p className="fd-turn-stat">
              {state.diff.summary.files_changed}{' '}
              {state.diff.summary.files_changed === 1 ? 'file' : 'files'}{' '}
              <span className="fd-add">+{state.diff.summary.insertions}</span>{' '}
              <span className="fd-del">−{state.diff.summary.deletions}</span>
            </p>
            <ul className="fd-wall-files">
              {patchFiles(state.diff.patch)
                .slice(0, MAX_FILES)
                .map((path) => (
                  <li key={path}>{path}</li>
                ))}
            </ul>
          </>
        )
      ) : null}
      <h3 className="fd-wall-label">Last agent message</h3>
      {message === null ? (
        <p className="fd-wall-note">
          {timeline?.ready === true ? 'No agent message yet.' : 'Loading the timeline…'}
        </p>
      ) : (
        <p
          className="fd-wall-message"
          title={agentLine(message) === message ? undefined : message.trim()}
        >
          {agentLine(message)}
        </p>
      )}
      {/* The cost as on every surface (§5 Cost); absent when the station does not report it. */}
      {cost === null ? null : (
        <p className="fd-wall-note fd-wall-cost">{`${cost} ${API_PRICES}`}</p>
      )}
    </li>
  );
}

/** A column for a wall display: the change summary and the last message come from display routes. */
function DisplayColumn({
  card,
  stations,
  timeline,
  env,
}: {
  readonly card: DeckSession;
  readonly stations: readonly StationView[];
  readonly timeline: Timeline | undefined;
  readonly env: DisplayEnv;
}) {
  const s = card.summary;
  const state = useDisplayChanges(env, card.station_id, s.session_id);
  const message = timeline?.ready === true ? lastDisplayMessage(timeline.display ?? []) : null;
  const cost = costText(s.usage);
  return (
    <li
      className="fd-wall-col"
      data-session={s.session_id}
      data-status={card.status}
      data-ref={refOf(card)}
    >
      <h2 className="fd-wall-h">{sessionTitle(card)}</h2>
      <p className="fd-wall-note">
        <span className="fd-dot" data-status={card.status} aria-hidden="true" />{' '}
        {STATUS_LABEL[card.status]} · {stationName(card.station_id, stations)}
        {s.batch === undefined ? '' : ` · ${s.batch.index} of ${s.batch.size}`}
      </p>
      <h3 className="fd-wall-label">Changes</h3>
      {state.kind === 'loading' ? <p className="fd-wall-note">Loading the changes…</p> : null}
      {state.kind === 'error' ? <p className="fd-wall-note">{state.text}</p> : null}
      {state.kind === 'ready' ? (
        state.changes.files.length === 0 && !hasChanges(state.changes.summary) ? (
          <p className="fd-wall-note">No file changes since the session started.</p>
        ) : (
          <>
            <p className="fd-turn-stat">
              {state.changes.summary.files_changed}{' '}
              {state.changes.summary.files_changed === 1 ? 'file' : 'files'}{' '}
              <span className="fd-add">+{state.changes.summary.insertions}</span>{' '}
              <span className="fd-del">−{state.changes.summary.deletions}</span>
            </p>
            <ChangeFiles changes={state.changes} max={MAX_FILES} />
          </>
        )
      ) : null}
      <h3 className="fd-wall-label">Last agent message</h3>
      {message === null ? (
        <p className="fd-wall-note">
          {timeline?.ready === true ? 'No agent message yet.' : 'Loading the timeline…'}
        </p>
      ) : (
        <p
          className="fd-wall-message"
          title={agentLine(message) === message ? undefined : message.trim()}
        >
          {agentLine(message)}
        </p>
      )}
      {/* The cost as on every surface (§5 Cost); absent when the station does not report it. */}
      {cost === null ? null : (
        <p className="fd-wall-note fd-wall-cost">{`${cost} ${API_PRICES}`}</p>
      )}
    </li>
  );
}

export function RacePane({
  batch,
  sessions,
  stations,
  timelines,
  watch,
  sessionActions,
  display,
}: {
  readonly display?: DisplayEnv | undefined;
  readonly batch: string;
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly timelines: Readonly<Record<string, Timeline>> | undefined;
  readonly watch: ((key: SessionKey) => () => void) | undefined;
  readonly sessionActions: ((key: SessionKey) => SessionActions) | undefined;
}) {
  const cards = siblings(sessions, batch).slice(0, RACE_COLUMNS);
  useTimelines(cards, watch);
  if (cards.length === 0)
    return <p className="fd-wall-note">This batch has no session on the board.</p>;
  return (
    <ul className="fd-wall-cols" aria-label="Sessions of the batch" data-count={cards.length}>
      {cards.map((card) =>
        display === undefined ? (
          <Column
            key={refOf(card)}
            card={card}
            stations={stations}
            timeline={timelineOf(timelines, keyOf(card))}
            actions={sessionActions?.(keyOf(card))}
          />
        ) : (
          <DisplayColumn
            key={refOf(card)}
            card={card}
            stations={stations}
            timeline={timelineOf(timelines, keyOf(card))}
            env={display}
          />
        ),
      )}
    </ul>
  );
}
