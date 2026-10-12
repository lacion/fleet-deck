import { useEffect, useState } from 'react';
import type { SessionKey, StationView, WorkspaceListing } from '../gen';
import type { Freshness } from '../runtime/state';
import {
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
  const ready =
    live && canDrive && !busy && station !== undefined && provider !== undefined && cwd !== null;

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
            const spec = {
              provider_instance: provider.instance,
              cwd,
              mode: modes.includes(mode) ? mode : null,
              model: model.trim() === '' ? null : model.trim(),
              title: title.trim() === '' ? null : title.trim(),
            };
            void actions
              .start(station.station_id, spec, prompt, importId.trim())
              .then((result) => {
                setOutcome(result);
                if (result.ok) {
                  setPrompt('');
                  setImportId('');
                }
              })
              .finally(() => setBusy(false));
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
          </p>
          {importProblem === null ? null : <p role="alert">{importProblem}</p>}
          <button type="submit" disabled={!ready || importProblem !== null}>
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
