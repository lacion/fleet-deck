// FG-10 (design §13.1, "Board"): the group "Other stations" of the Branches view: the `fd/*`
// branches that other stations pushed to the remote of a synced project.
import { useEffect, useRef, useState } from 'react';
import { formatHash } from '../app/routes';
import { relativeAge } from '../app/sessions';
import type { FetchAnswer } from '../gen';
import type { OtherBranch, OtherBranches } from '../integrations/actions';
import { useIntegrations, useProjects } from '../integrations/context';
import { message } from '../integrations/useLoad';

/** The session that shows the group: its project, station and id. */
export type FleetSession = {
  readonly projectId: string;
  readonly station: string;
  readonly session: string;
};

export type GroupState =
  | { readonly kind: 'loading' }
  | { readonly kind: 'error'; readonly message: string }
  | { readonly kind: 'ready'; readonly data: OtherBranches };

const EMPTY = 'No branches of other stations. Push is off, or no other station pushed yet.';

/** The branches of other stations only: the branches of the own station are not in this group. */
export const foreignBranches = (
  rows: readonly OtherBranch[],
  station: string,
): readonly OtherBranch[] => rows.filter((r) => r.station_id !== station);

/** "Fetched 5m ago", or "Fetched just now". */
export function fetchedText(at: number, now: number): string {
  const age = relativeAge(at, now);
  return age === 'now' ? 'Fetched just now' : `Fetched ${age} ago`;
}

/** The station column: the name, the id, "legacy branch", or "unknown station". */
export function stationText(row: OtherBranch): string {
  if (row.legacy) return 'legacy branch';
  return row.station_name ?? row.station_id ?? 'unknown station';
}

/** The text of a failed fetch: the `text` of the answer, else its `code`; `null` when it worked. */
export function fetchFailure(answer: FetchAnswer): string | null {
  if (answer.text !== undefined && answer.text !== '') return answer.text;
  return answer.code !== undefined && answer.code !== '' ? answer.code : null;
}

function Row({ row }: { readonly row: OtherBranch }) {
  const link = row.session;
  return (
    <li>
      <code className="fd-branch-name">{row.name}</code>
      {' · '}
      <span>{stationText(row)}</span>
      {' · '}
      {link === undefined ? (
        <span>no session</span>
      ) : (
        <a
          href={formatHash({
            name: 'session',
            station: link.station_id,
            session: link.session_id,
            tab: 'chat',
          })}
        >
          {link.callsign ?? link.title ?? link.session_id}
        </a>
      )}
      {' · '}
      <span className="fd-branch-count">
        ↑{row.ahead} ↓{row.behind}
      </span>
      {row.conflict === 'will_conflict' ? (
        <>
          {' · '}
          <span className="fd-badge" data-tone="hazard">
            Will conflict ({row.files.length} {row.files.length === 1 ? 'file' : 'files'})
          </span>
        </>
      ) : null}
    </li>
  );
}

/** The group as markup: the state, the rows, "Fetch" and the result of the last fetch. */
export function OtherStationsView({
  state,
  station,
  fetching,
  fetchError,
  onFetch,
  now,
}: {
  readonly state: GroupState;
  /** The station of the session: its own branches are not listed. */
  readonly station: string;
  readonly fetching: boolean;
  readonly fetchError: string | null;
  readonly onFetch: () => void;
  /** Unix ms of "now" (tests). */
  readonly now: number;
}) {
  const rows = state.kind === 'ready' ? foreignBranches(state.data.branches, station) : [];
  return (
    <section className="fd-since" aria-label="Other stations" data-view="other-stations">
      <h3 className="fd-label">Other stations</h3>
      {state.kind === 'loading' ? (
        <p className="fd-note">Loading the branches of other stations…</p>
      ) : (
        <p className="fd-note">
          <button type="button" className="fd-sm" disabled={fetching} onClick={onFetch}>
            {fetching ? 'Fetching…' : 'Fetch'}
          </button>
          {state.kind === 'ready' && state.data.fetched_at !== undefined ? (
            <> {fetchedText(state.data.fetched_at, now)}</>
          ) : null}
        </p>
      )}
      {fetchError === null ? null : (
        <p role="alert" className="fd-since-alert">
          {fetchError}
        </p>
      )}
      {state.kind === 'error' ? (
        <p role="alert" className="fd-since-alert">
          {state.message}
        </p>
      ) : null}
      {state.kind === 'ready' ? (
        <>
          {state.data.truncated ? (
            <p className="fd-note">Only the 200 newest branches are listed.</p>
          ) : null}
          {rows.length === 0 ? (
            <p className="fd-note">{EMPTY}</p>
          ) : (
            <ul className="fd-branches">
              {rows.map((row) => (
                <Row key={row.head + row.name} row={row} />
              ))}
            </ul>
          )}
        </>
      ) : null}
    </section>
  );
}

/**
 * The group of a session of a synced project. Nothing without the admin scope, without a project
 * with sync on, or for a deck without the route (an older deck).
 */
export function OtherStations({ fleet }: { readonly fleet: FleetSession }) {
  const env = useIntegrations();
  const project = useProjects().find((p) => p.project_id === fleet.projectId);
  const actions = env?.actions;
  const active = actions?.canAdmin === true && project?.git.sync === true;
  const [state, setState] = useState<GroupState | 'hidden'>({ kind: 'loading' });
  const [fetching, setFetching] = useState(false);
  const [fetchError, setFetchError] = useState<string | null>(null);
  const token = useRef(0);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const actionsRef = useRef(actions);
  actionsRef.current = actions;

  const load = () => {
    const calls = actionsRef.current;
    if (calls === undefined) return Promise.resolve();
    const mine = ++token.current;
    const current = () => alive.current && token.current === mine;
    return calls.projectBranches(fleet.projectId).then(
      (data) => {
        if (current()) setState(data === null ? 'hidden' : { kind: 'ready', data });
      },
      (e: unknown) => {
        if (current()) setState({ kind: 'error', message: message(e) });
      },
    );
  };
  // Once when the group shows; "Fetch" loads again. `load` only reads refs.
  useEffect(() => {
    if (active) void load();
  }, [active, fleet.projectId]);

  if (!active || actions === undefined || state === 'hidden') return null;
  const fetch = () => {
    setFetching(true);
    setFetchError(null);
    actions
      .fetchProject(fleet.projectId)
      .then(
        (answer) => {
          if (alive.current) setFetchError(fetchFailure(answer));
        },
        (e: unknown) => {
          if (alive.current) setFetchError(message(e));
        },
      )
      .then(load)
      .finally(() => {
        if (alive.current) setFetching(false);
      });
  };
  return (
    <OtherStationsView
      state={state}
      station={fleet.station}
      fetching={fetching}
      fetchError={fetchError}
      onFetch={fetch}
      now={Date.now()}
    />
  );
}
