import { useEffect, useRef, useState } from 'react';
import { formatHash } from '../app/routes';
import { basename, relativeAge } from '../app/sessions';
import type { DeckSession, StationView, WorktreeInfo, WorktreeVerdict } from '../gen';
import { ApiError } from '../runtime/api';
import type { Freshness } from '../runtime/state';
import { Check, Copy, Icon, RotateCcw } from '../ui/icons';
import { useNow } from '../ui/useNow';
import { ShellPanel } from '../shell/ShellPanel';
import { PairingDialog } from './PairingDialog';
import { authLabel, removeCommand, type StationActions } from './pairing';

type Props = {
  readonly stations: readonly StationView[];
  readonly sessions: readonly DeckSession[];
  readonly freshness: Freshness;
  readonly actions?: StationActions | undefined;
};

const NONE = '—';

const capitalize = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

/** The shortened key fingerprint with a button that copies the whole value. */
function Fingerprint({ value }: { readonly value: string }) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  const short = value.length > 12 ? `${value.slice(0, 12)}…` : value;
  return (
    <>
      <span title={value}>{short}</span>
      <button
        type="button"
        className="fd-iconbtn fd-sm"
        aria-label="Copy the key fingerprint"
        title="Copy the key fingerprint"
        onClick={() => {
          void navigator.clipboard?.writeText(value).then(() => {
            setCopied(true);
            clearTimeout(timer.current);
            timer.current = setTimeout(() => setCopied(false), 1500);
          });
        }}
      >
        <Icon of={copied ? Check : Copy} size={14} />
      </button>
    </>
  );
}

const VERDICTS: Record<WorktreeVerdict, { label: string; tone: string }> = {
  clean: { label: 'Clean', tone: 'ok' },
  has_work: { label: 'Has work', tone: 'act' },
  live: { label: 'Live', tone: 'info' },
  missing: { label: 'Missing', tone: 'muted' },
  unknown: { label: 'Unknown', tone: 'muted' },
};

const count = (n: number | null) => (n === null ? '?' : String(n));

/** The button that copies the command that removes a worktree. The board never runs it. */
function CopyCommand({ command }: { readonly command: string }) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  return (
    <button
      type="button"
      className="fd-ghost fd-sm"
      title={command}
      onClick={() => {
        void navigator.clipboard?.writeText(command).then(() => {
          setCopied(true);
          clearTimeout(timer.current);
          timer.current = setTimeout(() => setCopied(false), 1500);
        });
      }}
    >
      <Icon of={copied ? Check : Copy} size={14} /> Copy the remove command
    </button>
  );
}

/** One worktree: the verdict, the evidence, and the copy command. Read only. */
export function WorktreeRow({
  worktree: w,
  station,
  now,
}: {
  readonly worktree: WorktreeInfo;
  readonly station: string;
  readonly now: number;
}) {
  const verdict = VERDICTS[w.verdict];
  const command = removeCommand(w);
  const compared = w.compared_with === 'remotes' ? 'a remote' : 'main';
  return (
    <li className="fd-wt-row" data-verdict={w.verdict}>
      <div className="fd-wt-line">
        <span className="fd-badge fd-caps" data-tone={verdict.tone}>
          {verdict.label}
        </span>
        <span className="fd-wt-name" title={w.path}>
          {basename(w.path)}
        </span>
        {w.branch === null ? null : <span className="fd-badge">{w.branch}</span>}
        <a
          className="fd-wt-session"
          href={formatHash({
            name: 'session',
            station,
            session: w.session_id,
            tab: 'chat',
          })}
        >
          {w.title ?? w.session_id}
        </a>
      </div>
      <p className="fd-wt-evidence">
        {count(w.dirty)} changed · {count(w.ignored)} ignored · {count(w.unpushed)} commits not on{' '}
        {compared}
        {w.last_commit === null
          ? ''
          : ` · last commit “${w.last_commit.subject}” ${relativeAge(w.last_commit.at, now)}`}
      </p>
      {w.verdict === 'unknown' && w.note !== null ? <p className="fd-wt-note">{w.note}</p> : null}
      {!w.registered && w.exists ? (
        <p className="fd-wt-note" data-tone="act">
          Git does not list this worktree.
        </p>
      ) : null}
      {command === null ? null : <CopyCommand command={command} />}
    </li>
  );
}

type WorktreeState =
  | { readonly kind: 'idle' }
  | { readonly kind: 'loading' }
  | { readonly kind: 'ready'; readonly worktrees: readonly WorktreeInfo[] }
  | { readonly kind: 'error'; readonly text: string; readonly status: number | null };

const failure = (error: unknown): WorktreeState => ({
  kind: 'error',
  status: error instanceof ApiError ? error.status : null,
  text: error instanceof Error ? error.message : 'The worktrees did not load.',
});

/** The read-only worktree inventory of one station (DL-142); loads on the first open. */
function Worktrees({
  station,
  stationId,
  load,
  now,
}: {
  readonly station: string;
  readonly stationId: string;
  readonly load: (station: string) => Promise<{ readonly worktrees: readonly WorktreeInfo[] }>;
  readonly now: number;
}) {
  const [state, setState] = useState<WorktreeState>({ kind: 'idle' });
  const run = () => {
    setState({ kind: 'loading' });
    load(station).then(
      (list) => setState({ kind: 'ready', worktrees: list.worktrees }),
      (error: unknown) => setState(failure(error)),
    );
  };
  return (
    <details
      className="fd-wt"
      onToggle={(event) => {
        if (event.currentTarget.open && state.kind === 'idle') run();
      }}
    >
      <summary className="fd-label">
        Worktrees{state.kind === 'ready' ? ` (${state.worktrees.length})` : ''}
      </summary>
      <div className="fd-wt-body">
        <button
          type="button"
          className="fd-ghost fd-sm fd-wt-refresh"
          disabled={state.kind === 'loading'}
          onClick={run}
        >
          <Icon of={RotateCcw} size={14} /> Refresh
        </button>
        {state.kind === 'loading' ? <p className="fd-note">Loading…</p> : null}
        {state.kind === 'error' ? (
          state.status === 403 ? (
            <p className="fd-note">Only the owner can see worktrees.</p>
          ) : state.status === 502 ? (
            <p className="fd-note">This station does not report worktrees. Update the station.</p>
          ) : (
            <p role="alert">{state.text}</p>
          )
        ) : null}
        {state.kind === 'ready' ? (
          state.worktrees.length === 0 ? (
            <p className="fd-note">
              No worktrees. A session started with “A new worktree” gets one.
            </p>
          ) : (
            <>
              <ul className="fd-wt-list">
                {state.worktrees.map((w) => (
                  <WorktreeRow key={w.session_id} worktree={w} station={stationId} now={now} />
                ))}
              </ul>
              <p className="fd-note">
                The board does not remove worktrees. Git refuses to remove a worktree with changes;
                check “Has work” rows first.
              </p>
            </>
          )
        ) : null}
      </div>
    </details>
  );
}

/** Link health and board freshness are different facts. Never invent metadata. */
export function StationsPanel({ stations, sessions, freshness, actions }: Props) {
  const [adding, setAdding] = useState(false);
  const [confirm, setConfirm] = useState<string | null>(null);
  const [removeError, setRemoveError] = useState<string | null>(null);
  const now = useNow(undefined, 30000);
  const canPair = actions?.canAdmin === true && freshness === 'live';
  return (
    <section className="fd-stations" aria-label="Stations">
      <h2>
        Stations <span className="fd-count">{stations.length}</span>
      </h2>
      {actions === undefined ? null : actions.canAdmin ? (
        <button type="button" disabled={!canPair} onClick={() => setAdding(true)}>
          Add station
        </button>
      ) : (
        <p className="fd-note">Pairing needs the admin scope.</p>
      )}
      {adding && actions !== undefined ? (
        <PairingDialog actions={actions} onClose={() => setAdding(false)} />
      ) : null}
      {removeError === null ? null : <p role="alert">{removeError}</p>}
      {stations.length === 0 ? <p>No paired stations.</p> : null}
      <ul className="fd-station-list">
        {stations.map((station) => {
          const cards = sessions.filter((card) => card.station_id === station.station_id);
          const active = cards.filter((card) =>
            ['starting', 'running', 'stopping'].includes(card.summary.state),
          ).length;
          const info = station.info ?? null;
          const up = freshness !== 'stale' && station.reachable;
          return (
            <li
              className="fd-station fd-panel"
              key={station.station_id}
              data-station={station.station_id}
            >
              <div className="fd-station-head">
                <h3>{station.name}</h3>
                <span
                  className="fd-badge fd-health"
                  data-tone={up ? 'ok' : 'hazard'}
                  data-health={freshness === 'stale' ? 'stale' : station.reachable ? 'up' : 'down'}
                >
                  {freshness === 'stale'
                    ? 'Stale link status'
                    : station.reachable
                      ? 'Link up'
                      : 'Unreachable'}
                </span>
                {info?.version == null ? null : <span className="fd-badge">{info.version}</span>}
              </div>
              {freshness === 'stale' ? (
                <p className="fd-note">
                  Last reported: {station.reachable ? 'link up' : 'unreachable'}.
                </p>
              ) : null}
              <dl className="fd-facts">
                <dt>Station ID</dt>
                <dd>{station.station_id}</dd>
                <dt>Key fingerprint</dt>
                <dd>
                  <Fingerprint value={station.key} />
                </dd>
                <dt>Direction</dt>
                <dd>
                  {station.direction === 'deck_dials'
                    ? 'Deck dials station'
                    : station.direction === 'station_dials'
                      ? 'Station dials deck'
                      : NONE}
                </dd>
                <dt>Link address</dt>
                <dd>{station.dial_url ?? 'Dial-only (no stored address)'}</dd>
                <dt>Last seen</dt>
                <dd
                  title={
                    station.last_seen === null ||
                    Number.isNaN(new Date(station.last_seen).getTime())
                      ? undefined
                      : new Date(station.last_seen).toISOString()
                  }
                >
                  {station.last_seen === null ? NONE : relativeAge(station.last_seen, now)}
                </dd>
                <dt>Sessions</dt>
                <dd>{info?.max_sessions == null ? active : `${active} / ${info.max_sessions}`}</dd>
              </dl>
              <h4>Configured providers</h4>
              {info === null ? (
                <p className="fd-note">Reported when the station links to this deck.</p>
              ) : info.providers.length === 0 ? (
                <p className="fd-note">No provider configured.</p>
              ) : (
                <ul className="fd-providers">
                  {info.providers.map((p) => (
                    <li
                      key={p.instance}
                      className="fd-badge"
                      data-instance={p.instance}
                      data-tone={
                        p.auth === 'signed_in' ? 'ok' : p.auth === 'signed_out' ? 'hazard' : 'muted'
                      }
                      title={p.instance}
                    >
                      {capitalize(p.provider)}
                      {p.version === null ? '' : ` ${p.version}`} · {authLabel(p.auth)}
                    </li>
                  ))}
                </ul>
              )}
              {actions?.canAdmin === true && actions.shells !== undefined ? (
                info?.allow_shell === false ? (
                  <section className="fd-shells" aria-label={`Shells on ${station.name}`}>
                    <h4>Shells</h4>
                    <p className="fd-note">
                      Shells are off on this station. Set allow_shell = true in station.toml.
                    </p>
                  </section>
                ) : (
                  <ShellPanel
                    station={station.name}
                    actions={actions.shells}
                    disabled={!canPair || !station.reachable}
                  />
                )
              ) : null}
              {actions?.worktrees === undefined ? null : (
                <Worktrees
                  station={station.name}
                  stationId={station.station_id}
                  load={actions.worktrees}
                  now={now}
                />
              )}
              {actions?.canAdmin === true ? (
                <div className="fd-station-foot">
                  {confirm === station.name ? (
                    <p>
                      Remove {station.name}? Its link closes and its cards leave the board.{' '}
                      <button
                        type="button"
                        className="fd-danger fd-sm"
                        disabled={!canPair}
                        onClick={() => {
                          setConfirm(null);
                          setRemoveError(null);
                          void actions.remove(station.name).catch((e: unknown) => {
                            setRemoveError(e instanceof Error ? e.message : 'Remove failed');
                          });
                        }}
                      >
                        Confirm remove
                      </button>{' '}
                      <button type="button" className="fd-sm" onClick={() => setConfirm(null)}>
                        Cancel
                      </button>
                    </p>
                  ) : (
                    <button
                      type="button"
                      className="fd-danger fd-sm"
                      disabled={!canPair}
                      onClick={() => setConfirm(station.name)}
                    >
                      Remove station
                    </button>
                  )}
                </div>
              ) : null}
            </li>
          );
        })}
      </ul>
    </section>
  );
}
