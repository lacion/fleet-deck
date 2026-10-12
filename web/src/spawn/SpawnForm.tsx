import { useEffect, useState } from 'react';
import type { DeckSession, SessionKey, StationView, WorkspaceListing } from '../gen';
import type { Freshness } from '../runtime/state';
import {
  batchPrompts,
  importIdProblem,
  providerLabel,
  type SpawnActions,
  type SpawnOutcome,
  spawnModes,
  spawnTargets,
} from './spawn';

type Props = {
  readonly stations: readonly StationView[];
  readonly freshness: Freshness;
  readonly actions?: SpawnActions | undefined;
  readonly onOpen: (key: SessionKey) => void;
  /** The listing shown at the first render (tests). */
  readonly initialListing?: WorkspaceListing | null;
  /** The first prompt at the first render (a plan from the library, DL-301). */
  readonly initialPrompt?: string;
  /** DL-141: the station chosen at the first render (an unrouted task). */
  readonly initialStation?: string | undefined;
  /** DL-141: the fleet, for the recent directories of a station. */
  readonly sessions?: readonly DeckSession[] | undefined;
};

/** PLAN §11 spawn form for M2: station, provider, directory picker, mode,
 * model, title and first prompt. One button press sends one command. */
export function SpawnForm({
  stations,
  freshness,
  actions,
  onOpen,
  initialListing = null,
  initialPrompt = '',
}: Props) {
  const targets = spawnTargets(stations);
  const [stationId, setStationId] = useState(targets[0]?.station_id ?? '');
  const station = targets.find((s) => s.station_id === stationId);
  const providers = station?.info?.providers ?? [];
  const [instance, setInstance] = useState(providers[0]?.instance ?? '');
  const provider = providers.find((p) => p.instance === instance);
  const [listing, setListing] = useState<WorkspaceListing | null>(initialListing);
  const [cwd, setCwd] = useState<string | null>(null);
  const [browseError, setBrowseError] = useState<string | null>(null);
  const [mode, setMode] = useState('');
  const [model, setModel] = useState('');
  const [title, setTitle] = useState('');
  const [prompt, setPrompt] = useState(initialPrompt);
  const [importId, setImportId] = useState('');
  const [place, setPlace] = useState<'here' | 'worktree' | 'clone'>('here');
  const [cloneUrl, setCloneUrl] = useState('');
  const [cloneName, setCloneName] = useState('');
  const [batch, setBatch] = useState(false);
  const [coordinator, setCoordinator] = useState(false);
  const [busy, setBusy] = useState(false);
  const [outcome, setOutcome] = useState<SpawnOutcome | null>(null);

  // A station that goes away (or the first one that appears) resets the
  // choice; a directory belongs to its station.
  useEffect(() => {
    if (station === undefined && targets[0] !== undefined) setStationId(targets[0].station_id);
  }, [station, targets[0]?.station_id]);
  useEffect(() => {
    if (provider === undefined && providers[0] !== undefined) setInstance(providers[0].instance);
  }, [provider, providers[0]?.instance]);

  const live = freshness === 'live';
  const canDrive = actions?.canDrive === true;
  const browse = (path: string | null) => {
    if (actions === undefined || station === undefined) return;
    setBrowseError(null);
    actions.list(station.station_id, path).then(setListing, (error: unknown) => {
      setBrowseError(error instanceof Error ? error.message : 'The station did not answer.');
    });
  };
  const modes = spawnModes(provider);
  const importProblem = importId.trim() === '' ? null : importIdProblem(importId.trim());
  const cloneReady =
    place !== 'clone' ||
    importId.trim() !== '' ||
    (cloneUrl.trim() !== '' && cloneName.trim() !== '');
  const ready =
    live &&
    canDrive &&
    !busy &&
    station !== undefined &&
    provider !== undefined &&
    cwd !== null &&
    cloneReady;
  // Say why the start button is off: a disabled button alone looks like a
  // button that does nothing (owner report after M3).
  const missing = [
    live ? null : 'the board must be live',
    canDrive ? null : 'your login needs the drive scope',
    station === undefined ? 'choose a station' : null,
    provider === undefined ? 'choose a provider' : null,
    cwd === null
      ? 'choose a directory (Browse the station, open a folder, then Use this directory)'
      : null,
    cloneReady ? null : 'enter the clone URL and the new directory name',
  ].filter((m): m is string => m !== null);

  return (
    <section className="fd-spawn" aria-label="New session">
      <h2>New session</h2>
      {actions !== undefined && !canDrive ? (
        <p className="fd-note">Starting sessions needs the drive scope.</p>
      ) : null}
      {live ? null : <p className="fd-note">The form is off until the board is live.</p>}
      {targets.length === 0 ? (
        <p>No linked station has reported its providers yet.</p>
      ) : (
        <form
          onSubmit={(event) => {
            event.preventDefault();
            if (
              !ready ||
              actions === undefined ||
              station === undefined ||
              provider === undefined ||
              cwd === null ||
              importProblem !== null
            ) {
              return;
            }
            setBusy(true);
            setOutcome(null);
            const target =
              place === 'clone' && importId.trim() === '' ? `${cwd}/${cloneName.trim()}` : cwd;
            // An import continues a native session in this directory: it
            // ignores the workspace and batch choices (DL-308). A batch
            // forces a worktree per session (v0); a clone is one session.
            const native = importId.trim();
            const importing = native !== '';
            const spec = {
              provider_instance: provider.instance,
              cwd: target,
              mode: modes.includes(mode) ? mode : null,
              model: model.trim() === '' ? null : model.trim(),
              title: title.trim() === '' ? null : title.trim(),
              // The deck creates the grant and fills in its id, expiry and
              // control skill (ADR-006 §4, DL-217); 0 means its default.
              ...(coordinator && !importing
                ? {
                    coordinator: {
                      grant_id: '',
                      classes: ['read', 'mail', 'drive'],
                      idle_ttl_ms: 0,
                      expires_at: 0,
                      control_skill: { version: '', text: '' },
                    },
                  }
                : {}),
            };
            const workspace = importing
              ? null
              : place === 'clone'
                ? { kind: 'clone' as const, url: cloneUrl.trim() }
                : place === 'worktree' || batch
                  ? { kind: 'worktree' as const }
                  : null;
            const prompts =
              batch && place !== 'clone' && !importing ? batchPrompts(prompt) : [prompt];
            const id = station.station_id;
            void (async () => {
              let last: SpawnOutcome | null = null;
              let started = 0;
              for (const p of prompts) {
                last = await actions.start(id, spec, p, workspace, importing ? native : undefined);
                if (!last.ok) break;
                started += 1;
              }
              if (last !== null && prompts.length > 1) {
                last = {
                  ...last,
                  text: `${started} of ${prompts.length} sessions started. ${last.text}`,
                };
              }
              setOutcome(last);
              if (last?.ok === true) {
                setPrompt('');
                setImportId('');
                // One new session: go to it at once (DL-141). A batch stays on the form.
                if (prompts.length === 1 && last.sessionId !== null) {
                  onOpen({ station_id: id, session_id: last.sessionId });
                }
              }
            })().finally(() => setBusy(false));
          }}
        >
          <label>
            Station
            <select
              value={stationId}
              onChange={(event) => {
                setStationId(event.target.value);
                setListing(null);
                setCwd(null);
                setMode('');
              }}
            >
              {targets.map((s) => (
                <option key={s.station_id} value={s.station_id}>
                  {s.name}
                </option>
              ))}
            </select>
          </label>
          <label>
            Provider
            <select
              value={instance}
              onChange={(event) => {
                setInstance(event.target.value);
                setMode('');
              }}
            >
              {providers.map((p) => (
                <option key={p.instance} value={p.instance}>
                  {providerLabel(p)}
                </option>
              ))}
            </select>
          </label>
          <fieldset className="fd-picker" aria-label="Directory">
            <legend>Directory</legend>
            <p data-cwd>{cwd === null ? 'No directory chosen.' : `Chosen: ${cwd}`}</p>
            {listing === null ? (
              <button type="button" disabled={!live || !canDrive} onClick={() => browse(null)}>
                Browse the station
              </button>
            ) : (
              <>
                <p className="fd-path">{listing.path ?? 'Allowed roots'}</p>
                <div className="fd-picker-actions">
                  {listing.path === null ? null : (
                    <button type="button" onClick={() => browse(listing.parent)}>
                      {listing.parent === null ? 'Back to the roots' : 'Up one level'}
                    </button>
                  )}
                  {listing.path === null ? null : (
                    <button type="button" onClick={() => setCwd(listing.path)}>
                      Use this directory
                    </button>
                  )}
                </div>
                {listing.entries.length === 0 ? <p>No sub-directories.</p> : null}
                <ul>
                  {listing.entries.map((entry) => (
                    <li key={entry.path}>
                      <button
                        type="button"
                        aria-label={`Open ${entry.name}`}
                        onClick={() => browse(entry.path)}
                      >
                        {entry.name}
                        {entry.repository ? <span className="fd-tag">git</span> : null}
                      </button>
                    </li>
                  ))}
                </ul>
                {listing.truncated ? (
                  <p className="fd-note">The station shows only the first entries.</p>
                ) : null}
              </>
            )}
            {browseError === null ? null : <p role="alert">{browseError}</p>}
          </fieldset>
          <label>
            Workspace
            <select
              value={place}
              onChange={(event) => setPlace(event.target.value as 'here' | 'worktree' | 'clone')}
            >
              <option value="here">Work in this directory</option>
              <option value="worktree">A new worktree on its own branch</option>
              <option value="clone">Clone a URL into a new directory here</option>
            </select>
          </label>
          {place === 'clone' ? (
            <>
              <label>
                Clone URL
                <input value={cloneUrl} onChange={(event) => setCloneUrl(event.target.value)} />
              </label>
              <label>
                New directory name
                <input value={cloneName} onChange={(event) => setCloneName(event.target.value)} />
              </label>
            </>
          ) : (
            <label className="fd-check">
              <input
                type="checkbox"
                checked={batch}
                onChange={(event) => setBatch(event.target.checked)}
              />
              Batch: one session in its own worktree for each line (3x task starts three)
            </label>
          )}
          <label className="fd-check">
            <input
              type="checkbox"
              checked={coordinator}
              onChange={(event) => setCoordinator(event.target.checked)}
            />
            Coordinator: this session may read the fleet, send mail and drive the sessions it starts
          </label>
          <label>
            Permission mode
            <select value={mode} onChange={(event) => setMode(event.target.value)}>
              <option value="">Station default</option>
              {modes.map((m) => (
                <option key={m} value={m}>
                  {m}
                </option>
              ))}
            </select>
          </label>
          <label>
            Model (optional)
            <input value={model} onChange={(event) => setModel(event.target.value)} />
          </label>
          <label>
            Title (optional)
            <input value={title} onChange={(event) => setTitle(event.target.value)} />
          </label>
          <label>
            First prompt (optional)
            <textarea rows={4} value={prompt} onChange={(event) => setPrompt(event.target.value)} />
          </label>
          <label>
            Import CLI session id (optional)
            <input
              value={importId}
              spellCheck={false}
              autoCapitalize="off"
              onChange={(event) => setImportId(event.target.value)}
            />
          </label>
          <p className="fd-note">
            Continue a Claude or Codex session that you started in a terminal. Close it there first.
            An import runs in the chosen directory and ignores the workspace and batch choices.
          </p>
          {importProblem === null ? null : <p role="alert">{importProblem}</p>}
          {busy || missing.length === 0 ? null : (
            <p className="fd-note" id="fd-spawn-missing" data-missing>
              To start: {missing.join('; ')}.
            </p>
          )}
          <button
            type="submit"
            disabled={!ready || importProblem !== null}
            aria-describedby={missing.length === 0 ? undefined : 'fd-spawn-missing'}
          >
            {busy ? 'Starting…' : importId.trim() === '' ? 'Start session' : 'Import session'}
          </button>
        </form>
      )}
      {outcome === null ? null : (
        <div className="fd-outcome">
          <p role={outcome.ok ? 'status' : 'alert'}>{outcome.text}</p>
          {outcome.sessionId !== null && station !== undefined ? (
            <button
              type="button"
              onClick={() =>
                onOpen({ station_id: station.station_id, session_id: outcome.sessionId ?? '' })
              }
            >
              Open the new session
            </button>
          ) : null}
        </div>
      )}
    </section>
  );
}
