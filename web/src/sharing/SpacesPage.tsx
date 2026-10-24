// R11.3 (DL-166): "Spaces" (owner only): named sets of sessions that one share covers.
import { useState } from 'react';
import { sessionTitle } from '../app/sessions';
import { stationName } from '../board/fleet';
import type { DeckSession, SpaceView, StationView } from '../gen';
import { Boxes, Icon, Plus } from '../ui/icons';
import { Loading } from '../ui/Loading';
import { EmptyState, ErrorNotice, Field, FormActions, FormGrid, Page } from '../ui/page';
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

/** One space as a table row: its sessions, "Add a session" and "Share space…". */
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
    <tr data-space={space.space_id}>
      <td className="fd-cell-name">{space.name}</td>
      <td data-label="Sessions">
        {members.length === 0 ? (
          <span className="fd-note">No sessions in this space yet.</span>
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
      </td>
      <td data-label="Add">
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
      </td>
      <td className="fd-cell-actions">
        <button type="button" className="fd-ghost fd-sm" onClick={onShare}>
          Share space…
        </button>
      </td>
    </tr>
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
  /** The name form is open at the first render (tests). */
  readonly initialCreating?: boolean;
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
  initialCreating = false,
}: ViewProps) {
  const [name, setName] = useState('');
  const [creating, setCreating] = useState(initialCreating);
  const trimmed = name.trim();
  let body: React.ReactNode;
  if (state.kind === 'loading') {
    body = <Loading text="Loading the spaces…" />;
  } else if (state.kind === 'error') {
    body = <ErrorNotice text={state.text} onRetry={onRetry} />;
  } else if (state.value.length === 0) {
    body = (
      <EmptyState
        icon={Boxes}
        title="No spaces yet"
        body="Make a space, then add sessions to it."
        action={
          creating ? undefined : (
            <button type="button" className="fd-primary" onClick={() => setCreating(true)}>
              <Icon of={Plus} /> New space
            </button>
          )
        }
      />
    );
  } else {
    // The table sits on the canvas (§4.7: no box inside the page frame).
    body = (
      <div className="fd-tablewrap">
        <table className="fd-table" aria-label="Spaces">
          <thead>
            <tr>
              <th scope="col">Space</th>
              <th scope="col">Sessions</th>
              <th scope="col">Add</th>
              <th scope="col">
                <span className="fd-vh">Actions</span>
              </th>
            </tr>
          </thead>
          <tbody>
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
          </tbody>
        </table>
      </div>
    );
  }
  // One primary per screen (§2.2): while the list is empty, the empty state holds New space.
  const empty = state.kind === 'ok' && state.value.length === 0;
  return (
    <Page
      title="Spaces"
      count={state.kind === 'ok' && state.value.length > 0 ? state.value.length : undefined}
      back={{ href: '#/shares', label: 'Shares' }}
      wide
      description="A space is a named set of sessions. One share of a space covers all of them, also the ones that you add later."
      actions={
        creating || empty ? null : (
          <button type="button" className="fd-primary" onClick={() => setCreating(true)}>
            <Icon of={Plus} /> New space
          </button>
        )
      }
    >
      {creating ? (
        <div className="fd-panel fd-formpanel">
          <form
            className="fd-space-new"
            aria-label="New space"
            onSubmit={(e) => {
              e.preventDefault();
              if (trimmed !== '') {
                onCreate?.(trimmed);
                setName('');
                setCreating(false);
              }
            }}
          >
            <h2 className="fd-form-title">New space</h2>
            <FormGrid>
              <Field label="Name">
                <input
                  type="text"
                  value={name}
                  maxLength={MAX_SPACE_NAME_CHARS}
                  autoComplete="off"
                  onChange={(e) => setName(e.target.value)}
                />
              </Field>
            </FormGrid>
            <FormActions>
              <button type="button" className="fd-ghost" onClick={() => setCreating(false)}>
                Cancel
              </button>
              <button type="submit" className="fd-primary" disabled={busy || trimmed === ''}>
                Create space
              </button>
            </FormActions>
          </form>
        </div>
      ) : null}
      {error === null ? null : <ErrorNotice text={error} />}
      {body}
    </Page>
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
      <Page title="Spaces" back={{ href: '#/shares', label: 'Shares' }} wide>
        <p>Only the owner of the deck sees the spaces.</p>
        <a href="#/">Back to the fleet</a>
      </Page>
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
  );
}
