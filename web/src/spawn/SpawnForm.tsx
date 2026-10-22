import { useEffect, useRef, useState } from 'react';
import { instanceLimit, rateMeters } from '../board/usage';
import { batchRef, newBatchId } from '../compare/compare';
import type { Prefer, ProviderKind, TranscriptPin } from '../gen';
import { useProjects } from '../integrations/context';
import { DECK } from '../settings/actions';
import { useSettings } from '../settings/context';
import { FolderPicker } from '../ui/FolderPicker';
import { useNow } from '../ui/useNow';
import { resourceWarnings } from '../board/resources';
import { AgentFields } from './AgentFields';
import { BranchFields } from './BranchFields';
import { BrowseSessions } from './BrowseSessions';
import { CloneAccess } from './CloneAccess';
import { FleetControlFields } from './FleetControlFields';
import { TargetStations } from './TargetStations';
import {
  type FleetControl,
  fleetBlocked,
  fleetElevation,
  grantFields,
  NO_FLEET,
  showFleetFields,
} from './fleetControl';
import { KindChoice, PlacementFields, StationChoice, TypedDirectory } from './PlacementFields';
import type { NativeFill } from './nativeSessions';
import { type CloneDefaults, cloneDefaultsOf, DEFAULT_CLONE, resolveRepo } from './repoRef';
import { SetupField } from './SetupField';
import { CloneFields, PropRow, Seg, type SegOption, SheetHead, SpawnFoot } from './SpawnSheet';
import {
  AUTO,
  autoOffered,
  baseProblem,
  batchPrompts,
  branchProblem,
  importIdProblem,
  knownModels,
  limitBadge,
  MAX_BATCH_REF,
  moreOptionsOpen,
  type PlacedStart,
  placeStarts,
  projectOfStart,
  providerKinds,
  type PreflightResult,
  readSetupCmds,
  rememberSetupCmd,
  setupKey,
  setupProblem,
  startHomes,
  type SpawnOutcome,
  spawnModes,
  spawnTargets,
  versionWarning,
  workspaceRequest,
} from './spawn';
import { coarsePointer, type SpawnFormProps } from './spawnFormProps';
import { useFleetElevation } from './useFleetElevation';

/** The id of the note under the directory that says what is missing. */
const NEED_ID = 'fd-spawn-need';

/** The workspace choices: a short segment, the full sentence as its tooltip (DL-144). */
const PLACES: readonly SegOption<'here' | 'worktree' | 'clone'>[] = [
  { value: 'here', label: 'This directory', title: 'Work in this directory' },
  { value: 'worktree', label: 'New worktree', title: 'A new worktree on its own branch' },
  {
    value: 'clone',
    label: 'Clone repository',
    title: 'Clone a repository into a new directory here',
  },
];

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
  initialRepo = '',
  sessions = [],
  now: givenNow,
}: SpawnFormProps) {
  const targets = spawnTargets(stations);
  // FG-5 (§8.5): the form that "New session in this project" opened picks a home, not a station.
  const projects = useProjects();
  const [project] = useState(() => projectOfStart(projects, initialStation, initialCwd));
  const homes = project === undefined ? [] : startHomes(project, targets);
  const placeable = autoOffered(targets) && actions?.place !== undefined;
  const [stationId, setStationId] = useState(
    targets.some((s) => s.station_id === initialStation)
      ? (initialStation ?? '')
      : (homes[0]?.station_id ?? targets[0]?.station_id ?? ''),
  );
  const auto = stationId === AUTO;
  const station = targets.find((s) => s.station_id === stationId);
  const providers = station?.info?.providers ?? [];
  // FG-6 (§9.3): the placement choices under "Auto (best station)".
  const kinds = providerKinds(targets);
  const [kind, setKind] = useState<ProviderKind>(kinds[0] ?? 'claude');
  const [needs, setNeeds] = useState('');
  const [prefer, setPrefer] = useState<Prefer[]>([]);
  const [spread, setSpread] = useState(false);
  const resourceTexts = resourceWarnings(station?.info?.resources).map((w) => w.text);
  const [instance, setInstance] = useState(providers[0]?.instance ?? '');
  const provider = providers.find((p) => p.instance === instance);
  const now = useNow(givenNow, 30000);
  const meters = rateMeters(sessions, stations);
  // R4.3: the tightest limit of the chosen instance, as a fuel badge from WARN_PERCENT on.
  const badge = limitBadge(instanceLimit(meters, stationId, instance), now);
  // The directory as typed or picked; `cwd` is the trimmed text, or `null` when it is empty.
  const [cwdText, setCwd] = useState<string | null>(initialCwd ?? null);
  const byHome = project !== undefined && homes.length > 0;
  const homeCwd = byHome && auto;
  const cwd = cwdText === null || cwdText.trim() === '' ? null : cwdText.trim();
  const [mode, setMode] = useState('');
  const [model, setModel] = useState('');
  const [title, setTitle] = useState('');
  const [prompt, setPrompt] = useState(initialPrompt);
  const [importId, setImportId] = useState('');
  // B7: the pin of a verified entry that "Continue it" chose; the import command sends it.
  // Any edit of the fields clears it.
  const [importPin, setImportPin] = useState<TranscriptPin | null>(null);
  const [browsing, setBrowsing] = useState(false);
  const [place, setPlace] = useState<'here' | 'worktree' | 'clone'>(initialPlace);
  const [repoText, setRepoText] = useState(initialRepo);
  // The error line of the field is an alert once the owner left the field or pressed Start.
  const [repoSeen, setRepoSeen] = useState(false);
  const [nameText, setNameText] = useState('');
  const [nameTouched, setNameTouched] = useState(false);
  // The default host, organization and transport come from the deck settings (owner only).
  const settingsEnv = useSettings();
  const [cloneDefaults, setCloneDefaults] = useState<CloneDefaults>(DEFAULT_CLONE);
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
  const [fleet, setFleet] = useState<FleetControl>(NO_FLEET);
  // The target stations of the coordinator grant, besides the session's own station.
  const [pickedTargets, setPickedTargets] = useState<readonly string[]>([]);
  const { ask, node: elevationNode } = useFleetElevation(settingsEnv);
  const [busy, setBusy] = useState(false);
  const [outcome, setOutcome] = useState<SpawnOutcome | null>(null);
  const [setupCmd, setSetupCmd] = useState('');
  const [setupRemember, setSetupRemember] = useState(false);
  // True once the user types in the setup field; a change of station or
  // directory then keeps the typed text instead of the remembered one.
  const setupEdited = useRef(false);
  // "More options" opens by itself when an option has a value; the person can open it too.
  const [optionsUser, setOptionsUser] = useState(false);

  // The deck settings are read once. A login that is not the owner's, or a failed read, keeps the
  // built-in defaults: the form then asks for owner/name or a URL.
  useEffect(() => {
    if (settingsEnv === undefined) return;
    let current = true;
    void settingsEnv.settings
      .view(DECK)
      .then((view) => {
        if (current) setCloneDefaults(cloneDefaultsOf(view));
      })
      .catch(() => {
        if (current) setCloneDefaults(DEFAULT_CLONE);
      });
    return () => {
      current = false;
    };
  }, []);
  // A station that goes away (or the first one that appears) resets the
  // choice; a directory belongs to its station.
  useEffect(() => {
    if (!auto && station === undefined && targets[0] !== undefined) {
      setStationId(targets[0].station_id);
    }
  }, [auto, station, targets[0]?.station_id]);
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
  const placing = auto && placeable;
  const setupBad = setupShown && setupProblem(setupCmd) !== null;
  // B7: browsing is the owner's (the deck answers 403 to every other login).
  const browse =
    settingsEnv !== undefined &&
    station !== undefined &&
    actions?.nativeList !== undefined &&
    actions.nativePreview !== undefined
      ? { list: actions.nativeList, preview: actions.nativePreview }
      : null;
  const fillFrom = (fill: NativeFill) => {
    setInstance(fill.instance);
    setCwd(fill.cwd);
    setImportId(fill.importId);
    setImportPin(fill.pin);
  };
  const importProblem = importId.trim() === '' ? null : importIdProblem(importId.trim());
  const optionsOpen = moreOptionsOpen(optionsUser, {
    setup: setupShown ? setupCmd : '',
    coordinator,
    importId,
  });
  const branchName = branch.trim();
  const branchFields = place === 'worktree' && !batch;
  const refProblem =
    branchFields &&
    branchName !== '' &&
    (branchProblem(branchName) !== null ||
      (base.trim() !== '' && baseProblem(base.trim()) !== null));
  const repo = resolveRepo(repoText, cloneDefaults);
  const cloneUrl = 'url' in repo ? repo.url : '';
  const repoError = 'error' in repo ? repo.error : null;
  // The directory name follows the repository until the owner types one.
  const cloneName = nameTouched ? nameText : 'name' in repo ? repo.name : '';
  // The access result belongs to the station and the URL that it checked.
  const accessResult =
    access !== null && access.stationId === stationId && access.url === cloneUrl
      ? access.result
      : null;
  const checkAccess = () => {
    if (actions === undefined || station === undefined || checking || cloneUrl === '') return;
    const id = station.station_id;
    const url = cloneUrl;
    setChecking(true);
    setAccess(null);
    void actions
      .preflight(id, url)
      .then((result) => setAccess({ stationId: id, url, result }))
      .finally(() => setChecking(false));
  };
  const fleetShown = showFleetFields(settingsEnv !== undefined, coordinator, importing);
  const targetIds = pickedTargets.filter(
    (id) => id !== stationId && stations.some((s) => s.station_id === id),
  );
  const fleetBad = fleetBlocked(coordinator, fleetShown, fleet);
  const cloneReady =
    place !== 'clone' || importId.trim() !== '' || (cloneUrl !== '' && cloneName.trim() !== '');
  const hasTarget = auto
    ? placing && kinds.includes(kind)
    : station !== undefined && provider !== undefined;
  const cwdOk = homeCwd || cwd?.startsWith('/') === true;
  const ready =
    live &&
    canDrive &&
    !busy &&
    hasTarget &&
    cwdOk &&
    cloneReady &&
    !setupBad &&
    !refProblem &&
    !fleetBad;
  // Say why the start button is off: a disabled button alone looks like a
  // button that does nothing (owner report after M3). Each reason shows once, where it belongs:
  // the board and the scope at the top, the directory under its field, a repository, branch,
  // setup or grant problem at its field; the footer names the rest. Nothing while a start runs.
  const dirNote =
    busy || cwdOk || byHome
      ? null
      : cwd === null
        ? 'Choose a folder to start.'
        : 'Type an absolute path (it starts with /).';
  const scopeNote = actions !== undefined && !canDrive;
  const missing = (
    busy
      ? []
      : [
          canDrive || scopeNote ? null : 'your login needs the drive scope',
          auto || station !== undefined ? null : 'choose a station',
          hasTarget || (!auto && station === undefined) ? null : 'choose a provider',
          cwdOk || dirNote !== null ? null : 'choose a directory',
          cloneReady || repoError !== null ? null : 'enter the new directory name',
        ]
  ).filter((m): m is string => m !== null);
  const describedBy =
    [
      scopeNote ? 'fd-spawn-scope' : null,
      live ? null : 'fd-spawn-live',
      dirNote === null ? null : NEED_ID,
      missing.length === 0 ? null : 'fd-spawn-missing',
    ]
      .filter((id) => id !== null)
      .join(' ') || undefined;
  const startable = ready && importProblem === null;

  // DL-308: a clone is one session: no Batch switch then.
  const batchShown = place !== 'clone';
  return (
    <section className="fd-spawn" aria-label="New session">
      <SheetHead station={auto ? undefined : station} />
      {scopeNote ? (
        <p className="fd-note" id="fd-spawn-scope">
          Starting sessions needs the drive scope.
        </p>
      ) : null}
      {live ? null : (
        <p className="fd-note" id="fd-spawn-live">
          The form is off until the board is live.
        </p>
      )}
      {targets.length === 0 ? (
        <p className="fd-empty">No linked station has reported its providers yet.</p>
      ) : (
        <form
          // ⌘↵ (Ctrl+↵) starts, as the kbd hint of the Start button says.
          onKeyDown={(event) => {
            if (event.key === 'Enter' && (event.metaKey || event.ctrlKey) && startable) {
              event.preventDefault();
              event.currentTarget.requestSubmit();
            }
          }}
          onSubmit={(event) => {
            event.preventDefault();
            setRepoSeen(true);
            if (
              !ready ||
              actions === undefined ||
              importProblem !== null ||
              (auto ? false : station === undefined || provider === undefined || cwd === null)
            ) {
              return;
            }
            setBusy(true);
            setOutcome(null);
            const target =
              place === 'clone' && importId.trim() === '' ? `${cwd}/${cloneName.trim()}` : cwd;
            const openStation = station?.station_id ?? '';
            // An import continues a native session in this directory: it
            // ignores the workspace and batch choices (DL-308). A batch
            // forces a worktree per session (v0); a clone is one session.
            const native = importId.trim();
            const spec = {
              provider_instance: provider?.instance ?? '',
              cwd: target ?? '',
              mode: modes.includes(mode) ? mode : null,
              model: model.trim() === '' ? null : model.trim(),
              title: title.trim() === '' ? null : title.trim(),
              // The deck creates the grant and fills in its id, expiry and
              // control skill (ADR-006 §4, DL-217); 0 means its default.
              ...(coordinator && !importing
                ? {
                    coordinator: {
                      grant_id: '',
                      ...grantFields(fleet, fleetShown ? targetIds : []),
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
            const id = openStation;
            const setup = setupShown ? setupCmd : undefined;
            const key = setupKey(id, cwd ?? '');
            // R1.4: one batch id for each press; the compare page groups the sessions by it.
            const batchId =
              prompts.length >= 2 && prompts.length <= MAX_BATCH_REF && !importing
                ? newBatchId()
                : null;
            void (async () => {
              const gate = await fleetElevation(
                { coordinator, importing, fleet, targets: fleetShown ? targetIds : [] },
                ask,
              );
              if (!gate.ok) {
                setOutcome({
                  ok: false,
                  text: 'Start cancelled: fleet control needs your confirmation.',
                  sessionId: null,
                });
                return;
              }
              // FG-6: under Auto, the deck chooses a station for each start (§9.3); the starts then
              // go out as today, one for each station.
              let plan: PlacedStart<typeof spec>[] = prompts.map(() => ({ station: id, spec }));
              if (auto) {
                const result = await placeStarts({
                  place: actions.place,
                  count: prompts.length,
                  needs,
                  prefer,
                  spread,
                  kind,
                  project,
                  targets,
                  cwd,
                  clone: place === 'clone' && importId.trim() === '' ? cloneName.trim() : null,
                  spec,
                });
                if (!result.ok) {
                  setOutcome({ ok: false, text: result.text, sessionId: null });
                  return;
                }
                plan = result.starts;
              }
              let last: SpawnOutcome | null = null;
              let started = 0;
              let lastStation = id;
              for (const [i, p] of prompts.entries()) {
                const at = plan[i] ?? { station: id, spec };
                lastStation = at.station;
                last = await actions.start(
                  at.station,
                  at.spec,
                  p,
                  workspace,
                  importing ? native : undefined,
                  setup,
                  batchId === null ? undefined : batchRef(batchId, i + 1, prompts.length),
                  gate.elevation,
                  importing && importPin !== null ? importPin : undefined,
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
                setImportPin(null);
                // One new session: go to it at once (DL-141). A batch stays on the form.
                if (prompts.length === 1 && last.sessionId !== null) {
                  onOpen({ station_id: lastStation, session_id: last.sessionId });
                }
              }
            })().finally(() => setBusy(false));
          }}
        >
          <div className="fd-spawn-compose">
            <label className="fd-spawn-title">
              <span className="fd-vh">Title (optional)</span>
              <input
                value={title}
                placeholder="Session title (optional)"
                onChange={(event) => setTitle(event.target.value)}
              />
            </label>
            <label className="fd-spawn-task">
              <span className="fd-vh">What should the agent do? (optional)</span>
              <textarea
                rows={6}
                placeholder="What should the agent do?"
                // A phone keeps its keyboard closed until the person taps the field.
                autoFocus={!coarsePointer()}
                value={prompt}
                onChange={(event) => setPrompt(event.target.value)}
              />
            </label>
            {batch && batchShown ? (
              <p className="fd-note">
                One session in its own worktree for each line. 3x task starts three.
              </p>
            ) : null}
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
          <div className="fd-spawn-props">
            <PropRow head="Agent">
              {auto ? (
                <KindChoice kinds={kinds} value={kind} onChange={setKind} />
              ) : (
                <AgentFields
                  providers={providers}
                  instance={instance}
                  onInstance={(next) => {
                    setInstance(next);
                    setImportPin(null);
                    setMode('');
                    setModel('');
                  }}
                  badge={badge}
                  fieldKey={`${stationId}/${instance}`}
                  model={model}
                  models={knownModels(sessions, stationId, instance)}
                  onModel={setModel}
                  modes={modes}
                  mode={mode}
                  onMode={setMode}
                />
              )}
              {mode === 'bypassPermissions' ? (
                <p className="fd-note fd-spawn-risk">Runs every tool without asking</p>
              ) : null}
              {versionWarning(provider) === null ? null : (
                <p className="fd-note" role="status" data-version-warning>
                  {versionWarning(provider)}
                </p>
              )}
            </PropRow>
            <PropRow head={byHome ? 'Home' : 'Station'}>
              <StationChoice
                homes={byHome ? homes : null}
                targets={targets}
                value={stationId}
                auto={placeable}
                onChange={(id) => {
                  setStationId(id);
                  setImportPin(null);
                  setCwd(homes.find((h) => h.station_id === id)?.path ?? null);
                  setMode('');
                  setModel('');
                }}
              />
              {resourceTexts.length > 0 ? (
                <p className="fd-note" role="status" data-resource-warning>
                  {`This station is low on resources: ${resourceTexts.join('; ')}. You can still start the session.`}
                </p>
              ) : null}
              {placing ? (
                <PlacementFields
                  needs={needs}
                  onNeeds={setNeeds}
                  prefer={prefer}
                  onPrefer={setPrefer}
                  batch={previewing}
                  spread={spread}
                  onSpread={setSpread}
                />
              ) : null}
            </PropRow>
            <PropRow head="Where">
              <Seg
                label="Workspace"
                value={place}
                options={PLACES}
                onChange={(next) => {
                  setPlace(next);
                  if (next !== 'clone') {
                    setNameTouched(false);
                    setNameText('');
                  }
                }}
              />
              {byHome ? null : auto ? (
                <TypedDirectory
                  value={cwdText ?? ''}
                  onChange={setCwd}
                  invalid={cwd !== null && !cwdOk}
                  describedBy={dirNote === null ? undefined : NEED_ID}
                />
              ) : (
                <div
                  className="fd-fieldwrap"
                  role="group"
                  aria-label="Directory"
                  aria-describedby={dirNote === null ? undefined : NEED_ID}
                >
                  <FolderPicker
                    stationId={stationId}
                    value={cwdText ?? ''}
                    onChange={(path) => {
                      setImportPin(null);
                      setCwd(path);
                    }}
                    list={actions?.list}
                    sessions={sessions}
                    label="Directory path"
                    placeholder="Choose a folder"
                    browseDisabled={!live || !canDrive}
                    initialOpen={initialListing !== null}
                    initialListing={initialListing}
                  />
                </div>
              )}
              {dirNote === null ? null : (
                <p className="fd-note fd-spawn-need" id={NEED_ID}>
                  {dirNote}
                </p>
              )}
              {branchFields ? (
                <BranchFields branch={branch} base={base} onBranch={setBranch} onBase={setBase} />
              ) : null}
              {place === 'clone' ? (
                <CloneFields
                  repoText={repoText}
                  onRepo={setRepoText}
                  onRepoSeen={() => setRepoSeen(true)}
                  access={
                    <CloneAccess
                      url={cloneUrl}
                      checking={checking}
                      result={accessResult}
                      onCheck={checkAccess}
                    />
                  }
                  repoError={repoError}
                  repoAlert={repoSeen}
                  cloneUrl={cloneUrl}
                  cloneName={cloneName}
                  onName={(name) => {
                    setNameTouched(true);
                    setNameText(name);
                  }}
                />
              ) : null}
            </PropRow>
          </div>
          <details className="fd-spawn-options" open={optionsOpen}>
            <summary
              className="fd-label"
              onClick={(event) => {
                event.preventDefault();
                setOptionsUser(!optionsOpen);
              }}
            >
              More options
            </summary>
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
                onChange={(event) => {
                  setCoordinator(event.target.checked);
                  if (!event.target.checked) {
                    setFleet(NO_FLEET);
                    setPickedTargets([]);
                  }
                }}
              />
              Coordinator: this session may read the fleet, send mail and drive the sessions it
              starts
            </label>
            {fleetShown ? <FleetControlFields fleet={fleet} onChange={setFleet} /> : null}
            {fleetShown ? (
              <TargetStations
                stations={stations}
                ownId={stationId}
                picked={targetIds}
                onChange={setPickedTargets}
              />
            ) : null}
            <label>
              Import CLI session id (optional)
              <input
                value={importId}
                spellCheck={false}
                autoCapitalize="off"
                data-native-pin={
                  importPin === null ? undefined : `${importPin.dev}/${importPin.ino}`
                }
                onChange={(event) => {
                  setImportId(event.target.value);
                  setImportPin(null);
                }}
              />
            </label>
            {browse === null ? null : (
              <button type="button" className="fd-spawn-browse" onClick={() => setBrowsing(true)}>
                Browse sessions on this station
              </button>
            )}
            <p className="fd-note">
              Continue a Claude or Codex session that you started in a terminal. Close it there
              first. An import runs in the chosen directory and ignores the workspace and batch
              choices.
            </p>
            {importProblem === null ? null : <p role="alert">{importProblem}</p>}
          </details>
          <SpawnFoot
            batchShown={batchShown}
            batch={batch}
            onBatch={setBatch}
            missing={missing}
            describedBy={describedBy}
            outcome={outcome}
            onOpenNew={
              outcome?.sessionId != null && station !== undefined
                ? () =>
                    onOpen({
                      station_id: station.station_id,
                      session_id: outcome.sessionId ?? '',
                    })
                : null
            }
            disabled={!startable}
            label={
              busy
                ? 'Starting…'
                : importing
                  ? 'Import session'
                  : previewing
                    ? `Start ${batchLines.length} sessions`
                    : 'Start session'
            }
          />
        </form>
      )}
      {browsing && browse !== null && station !== undefined ? (
        <BrowseSessions
          stationId={station.station_id}
          stationName={station.name}
          providers={providers}
          actions={browse}
          onClose={() => setBrowsing(false)}
          onContinue={fillFrom}
        />
      ) : null}
      {elevationNode}
    </section>
  );
}
