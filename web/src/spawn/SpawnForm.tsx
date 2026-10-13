import { useEffect, useRef, useState } from 'react';
import { instanceLimit, mostRoom, rateMeters } from '../board/usage';
import { batchRef, newBatchId } from '../compare/compare';
import type { DeckSession, SessionKey, StationView, WorkspaceListing } from '../gen';
import type { Freshness } from '../runtime/state';
import { FolderPicker } from '../ui/FolderPicker';
import { useNow } from '../ui/useNow';
import { BranchFields } from './BranchFields';
import { CloneAccess } from './CloneAccess';
import { ModelField } from './ModelField';
import { SetupField } from './SetupField';
import {
  baseProblem,
  batchPrompts,
  branchProblem,
  importIdProblem,
  knownModels,
  MAX_BATCH_REF,
  providerChoiceLabel,
  type PreflightResult,
  readSetupCmds,
  rememberSetupCmd,
  setupKey,
  setupProblem,
  type SpawnActions,
  type SpawnOutcome,
  spawnModes,
  spawnTargets,
  versionWarning,
  workspaceRequest,
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
  /** R3: the directory chosen at the first render ("Fix checks" of a project). */
  readonly initialCwd?: string | undefined;
  /** DL-176: the workspace choice at the first render ("New session in this project": worktree). */
  readonly initialPlace?: 'here' | 'worktree' | undefined;
  /** DL-141: the fleet, for the recent directories of a station. */
  readonly sessions?: readonly DeckSession[] | undefined;
  /** Unix ms of now for the reset times of the limits (tests). */
  readonly now?: number | undefined;
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
  initialStation,
  initialCwd,
  initialPlace = 'here',
  sessions = [],
  now: givenNow,
}: Props) {
  const targets = spawnTargets(stations);
  const [stationId, setStationId] = useState(
    targets.some((s) => s.station_id === initialStation)
      ? (initialStation ?? '')
      : (targets[0]?.station_id ?? ''),
  );
  const station = targets.find((s) => s.station_id === stationId);
  const providers = station?.info?.providers ?? [];
  const [instance, setInstance] = useState(providers[0]?.instance ?? '');
  const provider = providers.find((p) => p.instance === instance);
  const now = useNow(givenNow, 30000);
  const meters = rateMeters(sessions, stations);
  const limits = new Map(
    providers.map((p) => [p.instance, instanceLimit(meters, stationId, p.instance)]),
  );
  const roomiest = mostRoom(limits);
  // The directory as typed or picked; `cwd` is the trimmed text, or `null` when it is empty.
  const [cwdText, setCwd] = useState<string | null>(initialCwd ?? null);
  const cwd = cwdText === null || cwdText.trim() === '' ? null : cwdText.trim();
  const [mode, setMode] = useState('');
  const [model, setModel] = useState('');
  const [title, setTitle] = useState('');
  const [prompt, setPrompt] = useState(initialPrompt);
  const [importId, setImportId] = useState('');
  const [place, setPlace] = useState<'here' | 'worktree' | 'clone'>(initialPlace);
  const [cloneUrl, setCloneUrl] = useState('');
  const [cloneName, setCloneName] = useState('');
  const [branch, setBranch] = useState('');
  const [base, setBase] = useState('');
  const [access, setAccess] = useState<{
    readonly stationId: string;
    readonly url: string;
    readonly result: PreflightResult | null;
  } | null>(null);
  const [checking, setChecking] = useState(false);
  const [batch, setBatch] = useState(false);
  const [coordinator, setCoordinator] = useState(false);
  const [busy, setBusy] = useState(false);
  const [outcome, setOutcome] = useState<SpawnOutcome | null>(null);
  const [setupCmd, setSetupCmd] = useState('');
  const [setupRemember, setSetupRemember] = useState(false);
  // True once the user types in the setup field; a change of station or
  // directory then keeps the typed text instead of the remembered one.
  const setupEdited = useRef(false);

  // A station that goes away (or the first one that appears) resets the
  // choice; a directory belongs to its station.
  useEffect(() => {
    if (station === undefined && targets[0] !== undefined) setStationId(targets[0].station_id);
  }, [station, targets[0]?.station_id]);
  useEffect(() => {
    if (provider === undefined && providers[0] !== undefined) setInstance(providers[0].instance);
  }, [provider, providers[0]?.instance]);
  // DL-148: the setup command follows the station and the directory, unless
  // the user edited the field since the last change.
  useEffect(() => {
    if (!setupEdited.current) {
      const stored = cwd === null ? undefined : readSetupCmds()[setupKey(stationId, cwd)];
      setSetupCmd(stored ?? '');
      setSetupRemember(stored !== undefined);
    }
    setupEdited.current = false;
  }, [stationId, cwd]);

  const live = freshness === 'live';
  const canDrive = actions?.canDrive === true;
  const modes = spawnModes(provider);
  const batchLines = batchPrompts(prompt);
  const importing = importId.trim() !== '';
  // A clone is one session and an import ignores the batch (DL-308): no preview then.
  const previewing = batch && place !== 'clone' && !importing && batchLines.length >= 2;
  const setupShown = station?.info?.allow_setup_cmd === true && !importing;
  const setupBad = setupShown && setupProblem(setupCmd) !== null;
  const importProblem = importId.trim() === '' ? null : importIdProblem(importId.trim());
  const branchName = branch.trim();
  const branchFields = place === 'worktree' && !batch;
  const refProblem =
    branchFields &&
    branchName !== '' &&
    (branchProblem(branchName) !== null ||
      (base.trim() !== '' && baseProblem(base.trim()) !== null));
  // The access result belongs to the station and the URL that it checked.
  const accessResult =
    access !== null && access.stationId === stationId && access.url === cloneUrl.trim()
      ? access.result
      : null;
  const checkAccess = () => {
    if (actions === undefined || station === undefined || checking) return;
    const id = station.station_id;
    const url = cloneUrl.trim();
    setChecking(true);
    setAccess(null);
    void actions
      .preflight(id, url)
      .then((result) => setAccess({ stationId: id, url, result }))
      .finally(() => setChecking(false));
  };
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
    cwd?.startsWith('/') === true &&
    cloneReady &&
    !setupBad &&
    !refProblem;
  // Say why the start button is off: a disabled button alone looks like a
  // button that does nothing (owner report after M3).
  const missing = [
    live ? null : 'the board must be live',
    canDrive ? null : 'your login needs the drive scope',
    station === undefined ? 'choose a station' : null,
    provider === undefined ? 'choose a provider' : null,
    cwd?.startsWith('/') === true
      ? null
      : 'choose a directory (type an absolute path, or Browse… and Use this folder)',
    cloneReady ? null : 'enter the clone URL and the new directory name',
    branchFields && branchName !== '' && branchProblem(branchName) !== null
      ? 'fix the branch name'
      : null,
    branchFields && branchName !== '' && base.trim() !== '' && baseProblem(base.trim()) !== null
      ? 'fix the base'
      : null,
    setupBad ? 'fix the setup command' : null,
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
              : workspaceRequest(place, batch, cloneUrl, branch, base);
            const prompts = batch && place !== 'clone' && !importing ? batchLines : [prompt];
            const id = station.station_id;
            const setup = setupShown ? setupCmd : undefined;
            const key = setupKey(id, cwd);
            // R1.4: one batch id for each press; the compare page groups the sessions by it.
            const batchId =
              prompts.length >= 2 && prompts.length <= MAX_BATCH_REF && !importing
                ? newBatchId()
                : null;
            void (async () => {
              let last: SpawnOutcome | null = null;
              let started = 0;
              for (const [i, p] of prompts.entries()) {
                last = await actions.start(
                  id,
                  spec,
                  p,
                  workspace,
                  importing ? native : undefined,
                  setup,
                  batchId === null ? undefined : batchRef(batchId, i + 1, prompts.length),
                );
                if (!last.ok) break;
                if (started === 0 && setup !== undefined) {
                  if (!setupRemember) rememberSetupCmd(key, null);
                  else if (setup.trim() !== '') rememberSetupCmd(key, setup);
                }
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
          <div className="fd-panel">
            <h3 className="fd-label">Where</h3>
            <label>
              Station
              <select
                value={stationId}
                onChange={(event) => {
                  setStationId(event.target.value);
                  setCwd(null);
                  setMode('');
                  setModel('');
                }}
              >
                {targets.map((s) => (
                  <option key={s.station_id} value={s.station_id}>
                    {s.name}
                  </option>
                ))}
              </select>
            </label>
            <div className="fd-fieldwrap" role="group" aria-label="Directory">
              <span className="fd-fieldlabel">Directory</span>
              <FolderPicker
                stationId={stationId}
                value={cwdText ?? ''}
                onChange={(path) => setCwd(path)}
                list={actions?.list}
                sessions={sessions}
                label="Directory path"
                browseDisabled={!live || !canDrive}
                initialOpen={initialListing !== null}
                initialListing={initialListing}
              />
            </div>
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
            {branchFields ? (
              <BranchFields branch={branch} base={base} onBranch={setBranch} onBase={setBase} />
            ) : null}
            {place === 'clone' ? (
              <>
                <div className="fd-spawn-url">
                  <label>
                    Clone URL
                    <input value={cloneUrl} onChange={(event) => setCloneUrl(event.target.value)} />
                  </label>
                  <CloneAccess
                    url={cloneUrl}
                    checking={checking}
                    result={accessResult}
                    onCheck={checkAccess}
                  />
                </div>
                <label>
                  New directory name
                  <input value={cloneName} onChange={(event) => setCloneName(event.target.value)} />
                </label>
              </>
            ) : null}
          </div>
          <div className="fd-panel">
            <h3 className="fd-label">Agent</h3>
            <label>
              Provider
              <select
                value={instance}
                onChange={(event) => {
                  setInstance(event.target.value);
                  setMode('');
                  setModel('');
                }}
              >
                {providers.map((p) => (
                  <option key={p.instance} value={p.instance}>
                    {providerChoiceLabel(
                      p,
                      limits.get(p.instance) ?? null,
                      now,
                      p.instance === roomiest,
                    )}
                  </option>
                ))}
              </select>
            </label>
            {versionWarning(provider) === null ? null : (
              <p className="fd-note" role="status" data-version-warning>
                {versionWarning(provider)}
              </p>
            )}
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
            <ModelField
              key={`${stationId}/${instance}`}
              value={model}
              models={knownModels(sessions, stationId, instance)}
              onChange={setModel}
            />
          </div>
          <div className="fd-panel">
            <h3 className="fd-label">Task</h3>
            <label>
              Title (optional)
              <input value={title} onChange={(event) => setTitle(event.target.value)} />
            </label>
            <label>
              First prompt (optional)
              <textarea
                rows={6}
                value={prompt}
                onChange={(event) => setPrompt(event.target.value)}
              />
            </label>
            {place === 'clone' ? null : (
              <label className="fd-spawn-opt">
                <input
                  type="checkbox"
                  checked={batch}
                  onChange={(event) => setBatch(event.target.checked)}
                />
                Batch: one session in its own worktree for each line (3x task starts three)
              </label>
            )}
            {previewing ? (
              <div className="fd-batch-preview" aria-label="Sessions to start">
                <p className="fd-label">{batchLines.length} sessions, each in its own worktree</p>
                <ol>
                  {batchLines.map((line, i) => (
                    <li key={i} title={line}>
                      {line}
                    </li>
                  ))}
                </ol>
              </div>
            ) : null}
          </div>
          <div className="fd-panel">
            <h3 className="fd-label">Options</h3>
            {setupShown ? (
              <SetupField
                value={setupCmd}
                remember={setupRemember}
                onValue={(v) => {
                  setupEdited.current = true;
                  setSetupCmd(v);
                }}
                onRemember={setSetupRemember}
              />
            ) : null}
            <label className="fd-spawn-opt">
              <input
                type="checkbox"
                checked={coordinator}
                onChange={(event) => setCoordinator(event.target.checked)}
              />
              Coordinator: this session may read the fleet, send mail and drive the sessions it
              starts
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
              Continue a Claude or Codex session that you started in a terminal. Close it there
              first. An import runs in the chosen directory and ignores the workspace and batch
              choices.
            </p>
            {importProblem === null ? null : <p role="alert">{importProblem}</p>}
          </div>
          <div className="fd-spawn-submit">
            <div className="fd-spawn-status">
              {busy || missing.length === 0 ? null : (
                <p className="fd-note" id="fd-spawn-missing" data-missing>
                  To start: {missing.join('; ')}.
                </p>
              )}
              {outcome === null ? null : (
                <div className="fd-outcome">
                  <p role={outcome.ok ? 'status' : 'alert'}>{outcome.text}</p>
                  {outcome.sessionId !== null && station !== undefined ? (
                    <button
                      type="button"
                      onClick={() =>
                        onOpen({
                          station_id: station.station_id,
                          session_id: outcome.sessionId ?? '',
                        })
                      }
                    >
                      Open the new session
                    </button>
                  ) : null}
                </div>
              )}
            </div>
            <button
              type="submit"
              className="fd-primary"
              disabled={!ready || importProblem !== null}
              aria-describedby={missing.length === 0 ? undefined : 'fd-spawn-missing'}
            >
              {busy
                ? 'Starting…'
                : importing
                  ? 'Import session'
                  : previewing
                    ? `Start ${batchLines.length} sessions`
                    : 'Start session'}
            </button>
          </div>
        </form>
      )}
    </section>
  );
}
