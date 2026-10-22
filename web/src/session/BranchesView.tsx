// DL-191: the other branches of the repository of the session, and the committed diff of one.
import { useEffect, useRef, useState } from 'react';
import { relativeAge } from '../app/sessions';
import { Diff } from '../chat/Diff';
import type { RepoBranch, RepoBranches, SessionDiff } from '../gen';
import { ApiError } from '../runtime/api';
import { type FleetSession, OtherStations } from './OtherStations';

type Props = {
  readonly load: () => Promise<RepoBranches>;
  readonly loadDiff: (head: string) => Promise<SessionDiff>;
  readonly onOpenFile?: ((path: string) => void) | undefined;
  /** FG-10: the session that shows the group "Other stations"; absent: no group. */
  readonly fleet?: FleetSession | undefined;
};

/** A failed request: the deck's own message, and whether "Try again" can help. */
export type Failure = { readonly message: string; readonly retry: boolean };

export type ListState =
  | { readonly kind: 'loading' }
  | { readonly kind: 'error'; readonly failure: Failure }
  | { readonly kind: 'ready'; readonly data: RepoBranches };

export type DiffState =
  | { readonly kind: 'loading' }
  | { readonly kind: 'error'; readonly failure: Failure }
  | { readonly kind: 'ready'; readonly diff: SessionDiff };

const RETRY_CODES: readonly string[] = ['busy', 'timeout', 'station_unreachable'];

function failureOf(e: unknown): Failure {
  if (e instanceof ApiError) {
    return { message: e.message, retry: e.code !== null && RETRY_CODES.includes(e.code) };
  }
  return { message: e instanceof Error ? e.message : 'The request failed.', retry: false };
}

function Alert({ failure, onRetry }: { readonly failure: Failure; readonly onRetry: () => void }) {
  return (
    <p role="alert" className="fd-since-alert">
      {failure.message}
      {failure.retry ? (
        <>
          {' '}
          <button type="button" className="fd-sm" onClick={onRetry}>
            Try again
          </button>
        </>
      ) : null}
    </p>
  );
}

/** The `files +N −N` stat of a diff summary, as the other Changes views write it. */
function Stat({ summary }: { readonly summary: SessionDiff['summary'] }) {
  return (
    <>
      {summary.files_changed} {summary.files_changed === 1 ? 'file' : 'files'}
      {summary.insertions > 0 ? (
        <>
          {' '}
          <span className="fd-add">+{summary.insertions}</span>
        </>
      ) : null}
      {summary.deletions > 0 ? (
        <>
          {' '}
          <span className="fd-del">−{summary.deletions}</span>
        </>
      ) : null}
    </>
  );
}

function Row({ row, onOpen }: { readonly row: RepoBranch; readonly onOpen: () => void }) {
  return (
    <li>
      <button type="button" className="fd-branch" onClick={onOpen}>
        <span className="fd-branch-name">{row.name}</span>
        {row.ahead > 0 ? <span className="fd-branch-count">{row.ahead} ahead</span> : null}
        {row.behind > 0 ? <span className="fd-branch-count">{row.behind} behind</span> : null}
        {row.ahead === 0 ? <span className="fd-branch-merged">merged</span> : null}
        <span className="fd-branch-subject">{row.subject}</span>
        <span className="fd-branch-age">{relativeAge(row.at * 1000, Date.now())}</span>
      </button>
    </li>
  );
}

/** The diff of one branch, with the way back to the list. */
export function BranchDiffView({
  row,
  state,
  origin,
  onBack,
  onRetry,
  onOpenFile,
}: {
  readonly row: RepoBranch;
  readonly state: DiffState;
  /** The branch of the session, or `HEAD` when it is on none. */
  readonly origin: string;
  readonly onBack: () => void;
  readonly onRetry: () => void;
  readonly onOpenFile?: ((path: string) => void) | undefined;
}) {
  return (
    <div className="fd-since" data-view="branch-diff">
      <p className="fd-note">
        <button type="button" className="fd-ghost fd-sm" onClick={onBack}>
          Back to branches
        </button>
      </p>
      {state.kind === 'loading' ? <p className="fd-note">Loading the diff…</p> : null}
      {state.kind === 'error' ? <Alert failure={state.failure} onRetry={onRetry} /> : null}
      {state.kind === 'ready' ? (
        <>
          <p className="fd-turn-stat">
            {row.name}: <Stat summary={state.diff.summary} /> since it left {origin}
          </p>
          {state.diff.patch.trim() === '' ? (
            <p className="fd-note">No committed changes.</p>
          ) : (
            <Diff
              patch={state.diff.patch}
              truncated={state.diff.truncated}
              id={`branch-${row.head}`}
              onOpenFile={onOpenFile}
            />
          )}
        </>
      ) : null}
    </div>
  );
}

/** The header lines and the rows of the branch list. */
export function BranchListView({
  list,
  onOpen,
  onReload,
}: {
  readonly list: ListState;
  readonly onOpen: (row: RepoBranch) => void;
  readonly onReload: () => void;
}) {
  return (
    <div className="fd-since" data-view="branches">
      {list.kind === 'ready' ? (
        <>
          <p className="fd-note">
            Branches of this repository, newest first.{' '}
            {list.data.branch === null ? (
              'The session is not on a branch.'
            ) : (
              <>
                This session is on <code>{list.data.branch}</code>.
              </>
            )}{' '}
            <button type="button" className="fd-sm" onClick={onReload}>
              Refresh
            </button>
          </p>
          <p className="fd-note">Uncommitted changes in other folders are not shown.</p>
          {list.data.truncated ? (
            <p className="fd-note">Only the 200 newest branches are listed.</p>
          ) : null}
          {list.data.branches.length === 0 ? (
            <p className="fd-note">No other branches.</p>
          ) : (
            <ul className="fd-branches">
              {list.data.branches.map((row) => (
                <Row key={row.head + row.name} row={row} onOpen={() => onOpen(row)} />
              ))}
            </ul>
          )}
        </>
      ) : null}
      {list.kind === 'loading' ? <p className="fd-note">Loading the branches…</p> : null}
      {list.kind === 'error' ? <Alert failure={list.failure} onRetry={onReload} /> : null}
    </div>
  );
}

/** The branches other than the one of the session, newest first, and the diff of the one that was clicked. */
export function BranchesView({ load, loadDiff, onOpenFile, fleet }: Props) {
  const [list, setList] = useState<ListState>({ kind: 'loading' });
  const [picked, setPicked] = useState<{
    readonly row: RepoBranch;
    readonly state: DiffState;
  } | null>(null);
  const loadRef = useRef(load);
  loadRef.current = load;
  const diffRef = useRef(loadDiff);
  diffRef.current = loadDiff;
  const token = useRef(0);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const reload = () => {
    const mine = ++token.current;
    setList({ kind: 'loading' });
    loadRef.current().then(
      (data) => {
        if (alive.current && token.current === mine) setList({ kind: 'ready', data });
      },
      (e: unknown) => {
        if (alive.current && token.current === mine) {
          setList({ kind: 'error', failure: failureOf(e) });
        }
      },
    );
  };
  // Once on mount; "Refresh" and "Try again" load again.
  useEffect(reload, []);

  const open = (row: RepoBranch) => {
    const mine = ++token.current;
    setPicked({ row, state: { kind: 'loading' } });
    diffRef.current(row.head).then(
      (diff) => {
        if (alive.current && token.current === mine) {
          setPicked({ row, state: { kind: 'ready', diff } });
        }
      },
      (e: unknown) => {
        if (alive.current && token.current === mine) {
          setPicked({ row, state: { kind: 'error', failure: failureOf(e) } });
        }
      },
    );
  };

  const origin = list.kind === 'ready' ? (list.data.branch ?? 'HEAD') : 'HEAD';
  if (picked !== null) {
    return (
      <BranchDiffView
        row={picked.row}
        state={picked.state}
        origin={origin}
        onBack={() => {
          // A late answer of the diff must not show after "Back".
          token.current += 1;
          setPicked(null);
        }}
        onRetry={() => open(picked.row)}
        onOpenFile={onOpenFile}
      />
    );
  }
  return (
    <>
      <BranchListView list={list} onOpen={open} onReload={reload} />
      {fleet === undefined ? null : <OtherStations fleet={fleet} />}
    </>
  );
}
