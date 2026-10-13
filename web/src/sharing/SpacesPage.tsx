// R11.3 (DL-166): "Spaces" (owner only): named sets of sessions that one share covers.
import { useState } from 'react';
import { sessionTitle } from '../app/sessions';
import { stationName } from '../board/fleet';
import type { DeckSession, SpaceView, StationView } from '../gen';
import { canShare, usePeople } from './context';
import { sharesChanged } from './shareStore';
import { MAX_SPACE_NAME_CHARS, openSpaceShare } from './spaceShare';
import { type Loaded, useLoad } from './useLoad';

const message = (e: unknown) => (e instanceof Error ? e.message : 'The request failed.');

const sameKey = (a: { station_id: string; session_id: string }, s: DeckSession) =>
  a.station_id === s.station_id && a.session_id === s.summary.session_id;

type SpaceProps = {
  readonly space: SpaceView;
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly busy: boolean;
  readonly onAdd: (station: string, session: string) => void;
  readonly onShare: () => void;
};

/** One space: its sessions, "Add a session" and "Share space…". */
export function SpaceCard({ space, sessions, stations, busy, onAdd, onShare }: SpaceProps) {
  const members = space.sessions.map((key) => ({
    key,
    card: sessions.find((s) => sameKey(key, s)),
  }));
  const free = sessions.filter((s) => !space.sessions.some((key) => sameKey(key, s)));
  const [pick, setPick] = useState('');
  const chosen = free.find((s) => `${s.station_id}/${s.summary.session_id}` === pick);
  const label = (s: DeckSession) => `${stationName(s.station_id, stations)} / ${sessionTitle(s)}`;
  return (
    <li className="fd-space" data-space={space.space_id}>
      <div className="fd-space-head">
        <h2 className="fd-space-name">{space.name}</h2>
        <button type="button" className="fd-ghost fd-sm" onClick={onShare}>
          Share space…
        </button>
      </div>
      {members.length === 0 ? (
        <p className="fd-note">No sessions in this space yet.</p>
      ) : (
        <ul className="fd-space-sessions" aria-label={`Sessions of ${space.name}`}>
          {members.map(({ key, card }) => (
            <li key={`${key.station_id}/${key.session_id}`}>
              {card === undefined
                ? `${stationName(key.station_id, stations)} / ${key.session_id}`
                : label(card)}
            </li>
          ))}
        </ul>
      )}
      <form
        className="fd-space-add"
        aria-label={`Add a session to ${space.name}`}
        onSubmit={(e) => {
          e.preventDefault();
          if (chosen !== undefined) {
            onAdd(chosen.station_id, chosen.summary.session_id);
            setPick('');
          }
        }}
      >
        <label>
          <span className="fd-vh">Session to add to {space.name}</span>
          <select
            value={pick}
            onChange={(e) => setPick(e.target.value)}
            disabled={free.length === 0}
          >
            <option value="">{free.length === 0 ? 'No more sessions' : 'Pick a session…'}</option>
            {free.map((s) => (
              <option
                key={`${s.station_id}/${s.summary.session_id}`}
                value={`${s.station_id}/${s.summary.session_id}`}
              >
                {label(s)}
              </option>
            ))}
          </select>
        </label>
        <button type="submit" className="fd-sm" disabled={busy || chosen === undefined}>
          Add a session
        </button>
      </form>
    </li>
  );
}

type ViewProps = {
  readonly state: Loaded<readonly SpaceView[]>;
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly busy?: boolean;
  readonly error?: string | null;
  readonly onRetry?: () => void;
  readonly onCreate?: (name: string) => void;
  readonly onAdd?: (space: SpaceView, station: string, session: string) => void;
  readonly onShare?: (space: SpaceView) => void;
};

/** The page of one state: loading, error, the spaces with "New space". */
export function SpacesView({
  state,
  sessions,
  stations,
  busy = false,
  error = null,
  onRetry,
  onCreate,
  onAdd,
  onShare,
}: ViewProps) {
  const [name, setName] = useState('');
  const trimmed = name.trim();
  let body: React.ReactNode;
  if (state.kind === 'loading') {
    body = (
      <p role="status" className="fd-note">
        Loading the spaces…
      </p>
    );
  } else if (state.kind === 'error') {
    body = (
      <div className="fd-art-state">
        <p role="alert" className="fd-note fd-share-error">
          {state.text}
        </p>
        <button type="button" className="fd-ghost fd-sm" onClick={onRetry}>
          Try again
        </button>
      </div>
    );
  } else if (state.value.length === 0) {
    body = <p className="fd-empty">No spaces yet. Make one, then add sessions to it.</p>;
  } else {
    body = (
      <ul className="fd-spaces" aria-label="Spaces">
        {state.value.map((space) => (
          <SpaceCard
            key={space.space_id}
            space={space}
            sessions={sessions}
            stations={stations}
            busy={busy}
            onAdd={(station, session) => onAdd?.(space, station, session)}
            onShare={() => onShare?.(space)}
          />
        ))}
      </ul>
    );
  }
  return (
    <section className="fd-spaces-page" aria-label="Spaces">
      <h1 className="fd-screen-title">Spaces</h1>
      <p className="fd-note">
        A space is a named set of sessions. One share of a space covers all of them, also the ones
        that you add later.
      </p>
      <form
        className="fd-space-new"
        aria-label="New space"
        onSubmit={(e) => {
          e.preventDefault();
          if (trimmed !== '') {
            onCreate?.(trimmed);
            setName('');
          }
        }}
      >
        <label>
          Name
          <input
            type="text"
            value={name}
            maxLength={MAX_SPACE_NAME_CHARS}
            autoComplete="off"
            onChange={(e) => setName(e.target.value)}
          />
        </label>
        <button type="submit" className="fd-primary" disabled={busy || trimmed === ''}>
          New space
        </button>
      </form>
      {error === null ? null : (
        <p role="alert" className="fd-note fd-share-error">
          {error}
        </p>
      )}
      {body}
    </section>
  );
}

type Props = {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
};

/** The Spaces page. */
export function SpacesPage({ sessions, stations }: Props) {
  const people = usePeople();
  const calls = people.shares;
  const { state, reload, set } = useLoad<readonly SpaceView[]>(
    () => (calls === undefined ? Promise.resolve([]) : calls.spaces()),
    [calls],
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!canShare(people) || calls === undefined) {
    return (
      <div className="fd-screen">
        <p>Only the owner of the deck sees the spaces.</p>
        <a href="#/">Back to the fleet</a>
      </div>
    );
  }
  const run = async (call: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await call();
      const next = await calls.spaces();
      set(next);
      // The People tab and the mob strip of the owner follow the sessions of the spaces.
      sharesChanged();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="fd-screen">
      <SpacesView
        state={state}
        sessions={sessions}
        stations={stations}
        busy={busy}
        error={error}
        onRetry={reload}
        onCreate={(name) => void run(() => calls.createSpace(name))}
        onAdd={(space, station, session) =>
          void run(() => calls.addToSpace(space.space_id, station, session))
        }
        onShare={openSpaceShare}
      />
    </div>
  );
}
