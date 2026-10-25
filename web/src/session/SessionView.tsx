// DL-140: the session pane: header, the notice of a terminal hold, the tabs
// (Chat | Changes | Files), the chat with the approval dock and the composer.
import {
  type KeyboardEvent,
  type ReactNode,
  useCallback,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
} from 'react';
import { ArtifactsProvider, useArtifacts } from '../artifacts/context';
import { SessionArtifacts } from '../artifacts/SessionArtifacts';
import { AgentHead, AgentsMenu, subagentName } from '../chat/AgentsMenu';
import { ChatView } from '../chat/ChatView';
import { SubagentWork } from '../chat/SubagentWork';
import { WorkflowAgentView } from '../chat/WorkflowAgentView';
import { useSessionHistory } from '../chat/history';
import type { DeckSession, StationView } from '../gen';
import type { FleetActions } from '../fleet/fleet';
import type { Freshness, Timeline } from '../runtime/state';
import { isShared, usePeople } from '../sharing/context';
import { sessionKey } from '../runtime/state';
import { useShellChips } from '../chat/ShellChips';
import { usePresenceHeartbeat, useSessionPeople } from '../sharing/hooks';
import { roleAccess } from '../sharing/member';
import { useIntegrations } from '../integrations/context';
import { useReviewActions } from '../review/context';
import { commentOptions } from '../review/hashMenu';
import { useReview } from '../review/useReview';
import { MobStrip } from '../sharing/MobStrip';
import { SideChat } from '../sharing/SideChat';
import type { ShellActions } from '../shell/shell';
import type { Watch } from '../terminal/handover';
import type { TerminalStore } from '../terminal/store';
import { useAgentChoice } from './agentChoice';
import { ApprovalDock } from './ApprovalDock';
import { onConfirmed, type SessionActions } from './commands';
import { isHibernated, resumeThenSend } from '../views/hibernate';
import { Composer } from './Composer';
import { answerVia, dockRefusals, useSessionController } from './controller';
import { composerModelChoice } from './effortChoice';
import { useArtifactRows } from './artifactRows';
import { ArtifactsLink } from './ArtifactsLink';
import { FilesPanel } from './FilesPanel';
import { handoverFlow } from './handoverFlow';
import { HoldNotice } from './HoldNotice';
import { ExecutionDialog } from './ExecutionDialog';
import { useResumeCheck } from './resumeRefusal';
import { useFleetControl } from './FleetControlChip';
import { LandPanel } from './LandPanel';
import { MoveBanner } from './MoveBanner';
import { useMove } from './MoveDialog';
import { CONFIRM_LINE, composerHidden, confirmLineShown } from './moveModel';
import { disabledReason, useFilesOpen, useWide } from './paneLayout';
import { changeTotal, PropertiesPanel, usePropsOpen, useSessionChanges } from './PropertiesPanel';
import { SessionHeader } from './SessionHeader';
import { useTabReserve } from './tabReserve';
import {
  commandTasks,
  hasAgents,
  isLive,
  runningCount,
  shownStatus,
  taskStates,
  toolTitles,
} from './tasks';
import { buildTimeline, runningSubagents, sessionModels } from './timeline';
import { commandList } from './slash';
import { type FileRequest, RevertStatus, TurnsPanel } from './TurnsPanel';
import { workflowAgentTotal } from './workflows';
import { finalizedTurns, useDiffTotal } from './workspace';
import { modelText } from '../ui/format';
import { sessionRef } from '../ui/localSet';
import { appsEnvOf } from '../apps/env';

export { readFilesOpen } from './paneLayout';

export type PaneTab = 'chat' | 'changes' | 'files' | 'artifacts' | 'people';

export type SessionViewProps = {
  readonly session: DeckSession;
  readonly stations: readonly StationView[];
  readonly timeline: Timeline | undefined;
  readonly freshness: Freshness;
  readonly actions: SessionActions | undefined;
  /** `undefined`: no admin scope, so no terminal features. */
  readonly shells?: ShellActions | undefined;
  readonly terminals?: TerminalStore | undefined;
  /** The live summary of this session, for the handover flows. */
  readonly watch?: Watch | undefined;
  readonly layout?: 'desktop' | 'tablet' | 'phone';
  readonly tab?: PaneTab;
  readonly onTab?: (tab: PaneTab) => void;
  /** B6: the node id of the subagent that the chat tab shows (`?agent=`); unknown: the main chat. */
  readonly agent?: string;
  readonly onAgent?: (nodeId: string | null) => void;
  readonly onBack?: () => void;
  /** The phone shows the terminal screen. */
  readonly onTerminalOpened?: () => void;
  /** Opens a shell in the workspace; the session menu offers it when given. */
  readonly onNewShell?: (() => void) | undefined;
  /** The fleet calls: the session menu offers "Fleet access…" when given. */
  readonly fleet?: FleetActions | undefined;
};

const noop = () => {};

/** One session: header, tabs, the chat with its approval dock and composer. */
export function SessionView(props: SessionViewProps) {
  const { session, stations, timeline, freshness, actions, shells, terminals, watch } = props;
  const { layout = 'desktop', onTab = noop, onBack, onTerminalOpened, onNewShell } = props;
  const onAgent = props.onAgent ?? noop;
  const s = session.summary;
  const caps = s.capabilities;
  const key = useMemo(
    () => ({ station_id: session.station_id, session_id: s.session_id }),
    [session.station_id, s.session_id],
  );
  const controller = useSessionController(session, freshness, actions ?? null);
  const { flags, busy } = controller;
  // R11 (DL-166): a member is a guest of the owner's deck: its role decides what the pane offers.
  const env = usePeople();
  const guest = env.member;
  const access =
    guest === null ? { composer: true, questions: true, approvals: true } : roleAccess(guest.role);
  usePresenceHeartbeat(key);
  const people = useSessionPeople(key);
  const [driveNote, setDriveNote] = useState<{ ok: boolean; text: string } | null>(null);
  const [driveBusy, setDriveBusy] = useState(false);
  const drive = (call: () => Promise<{ ok: boolean; text: string }>) => {
    setDriveBusy(true);
    void call().then((r) => {
      setDriveNote(r.ok ? null : r);
      setDriveBusy(false);
    });
  };
  const events = timeline?.events ?? [];
  const tree = buildTimeline(events);
  const tasks = useMemo(() => taskStates(events), [events]);
  // B6, B12: only the node id of a subagent or `wf:<task>:<index>` selects an agent view; any other
  // value shows the main chat. A chosen background command is marked (`highlight`).
  const choice = useAgentChoice({
    session,
    actions,
    events,
    onAgent,
    subagents: tree.subagents,
    member: guest !== null,
    tab: props.tab,
    agent: props.agent,
  });
  const { agentSub, wf, workflows, highlight, loadSubagent } = choice;
  const titles = toolTitles(tree.items);
  // B8: the Claude shell results that the next prompt carries.
  const shellChips = useShellChips(sessionKey(key), tree.items, s.provider);
  const turns = finalizedTurns(events);
  const filesOffered = actions?.files != null;
  // DL-191: the Branches view of the Changes tab is the owner's.
  const branchesOffered = guest === null && actions?.branches != null;
  const wakes = isHibernated(session) && flags.canResume && guest === null;
  const wide = useWide();
  const [filesOpen, toggleFiles] = useFilesOpen();
  const sidePanel = layout === 'desktop' && wide && filesOffered;
  const sideOpen = sidePanel && filesOpen;
  const tabsOffered = filesOffered && !sideOpen;
  // §4.5: the properties panel takes the right rail while the Files panel does not.
  const [propsOpen, toggleProps] = usePropsOpen();
  const propsRail = layout === 'desktop' && wide && !sideOpen;
  const artifactsEnv = useArtifacts();
  const artifactsOffered = artifactsEnv !== undefined;
  // DL-179: review comments. The board hides what the deck refuses; the deck still decides.
  const reviewActions = useReviewActions();
  const review = useReview(key, session.review_rev, freshness === 'live');
  const self = guest === null ? 'owner' : guest.name;
  const canWriteReview =
    guest === null ? actions?.canDrive === true : roleAccess(guest.role).composer;
  const [focus, setFocus] = useState<{ threadId: string; nonce: number } | null>(null);
  const openComment = useCallback(
    (shortId: string) => {
      const found = review.byShort.get(shortId);
      if (found === undefined) return;
      setFocus({ threadId: found.thread_id, nonce: Date.now() });
      onTab('changes');
    },
    [review.byShort, onTab],
  );
  const commentChoices = useMemo(() => commentOptions(review.byShort.values()), [review.byShort]);
  const wanted: PaneTab = props.tab ?? 'chat';
  const peopleOffered = people.offered;
  const tab: PaneTab =
    (wanted === 'files' && !tabsOffered) ||
    (wanted === 'artifacts' && !artifactsOffered) ||
    (wanted === 'people' && !peopleOffered)
      ? 'chat'
      : wanted;
  // The artifact that the Artifacts tab shows; a chat chip and a publish set it.
  const [openArtifact, setOpenArtifact] = useState<{
    readonly id: string;
    readonly version: number | null;
  } | null>(null);
  const [kept, setKept] = useState<ReadonlySet<string>>(() => new Set());
  const flow =
    actions && shells && terminals && watch
      ? handoverFlow({ key, title: s.title ?? s.session_id, watch, actions, shells, terminals })
      : null;
  // DL-141: a station with `allow_shell = false` refuses the TUI handover.
  const shellsOff =
    stations.find((st) => st.station_id === session.station_id)?.info?.allow_shell === false;
  const terminal = flow
    ? {
        available: !shellsOff,
        onOpen: () =>
          controller.act('handover', async () => {
            const r = await flow.open();
            if (r.ok) onTerminalOpened?.();
            return r;
          }),
        onReturn: () => controller.act('return', flow.back),
      }
    : null;
  // B8: the owner types `!command` when shells are on and the provider runs them.
  const bang =
    guest === null &&
    actions != null &&
    !shellsOff &&
    (s.provider === 'claude' || s.provider === 'codex');
  const hold = s.terminal ?? null;
  const check = useResumeCheck(actions);
  // FG-9: the newest move of the session (owner only) feeds the move banner.
  const integrations = useIntegrations();
  const moveOwner = guest === null && integrations?.actions.canAdmin === true;
  const move = useMove(session, moveOwner);
  const ids = useId();
  const tabs = useRef<Record<string, HTMLButtonElement | null>>({});
  const tabRow = useRef<HTMLDivElement>(null);
  useTabReserve(tabRow);
  // A viewer has no drive scope but sees the files of the shared session.
  const filesReady = flags.live && (actions?.canDrive === true || guest !== null);
  const changesRef = sessionRef(session.station_id, s.session_id);
  const files = actions?.files ?? null;
  // §4.5: one size of the session's changes on every tab and layout: the Changes tab label (the
  // changed files) and the Properties Changes row. The session diff loads when the session opens;
  // a failed read falls back to the sum of the turn diffs.
  const hasBase = Boolean(s.base);
  const changesRead = useSessionChanges(
    changesRef,
    filesReady ? files : null,
    hasBase,
    turns.length,
  );
  const total = changeTotal(
    useDiffTotal(changesRef),
    turns,
    hasBase && filesReady && files !== null && changesRead === 'loading',
  );
  // Running agents: the loaded timeline, or the summary for one older than its pages (DL-189).
  const running = Math.max(runningCount(tree.subagents, tasks, workflows), runningSubagents(s));
  const agentsMenu = hasAgents(tree.subagents, tasks, workflows) ? (
    <AgentsMenu
      {...choice.menu}
      subagents={tree.subagents}
      tasks={tasks}
      toolTitles={titles}
      agent={agentSub?.node_id ?? null}
      onChoose={choice.chooseAgent}
      running={running}
    />
  ) : undefined;
  const artifactRows = useArtifactRows(
    artifactsEnv?.actions,
    session.station_id,
    s.session_id,
    s.artifacts_count,
  );
  const changedFiles = total?.files_changed ?? 0;
  const propsShown = propsRail && propsOpen;
  // ADR-008 §5.3: the fleet-control fact of a coordinator, in Properties or in the session menu.
  const fleetControl = useFleetControl(
    guest === null && actions?.canAdmin === true ? actions : null,
  );
  // The composer turns into a Resume bar; on the phone the header then shows no Resume of its own.
  const composerResume =
    flags.canResume && !wakes && guest === null && actions !== null && actions !== undefined;
  const composerShown =
    tab === 'chat' && choice.note === null && !composerHidden(s) && access.composer;
  const resumeBar = composerResume && composerShown;
  // G4: the requests that the dock shows (only on the Chat tab); the header shows the rest.
  const dockRequests =
    tab !== 'chat' || !access.questions
      ? []
      : s.open_requests.filter((r) => access.approvals || r.request_kind === 'question');
  const order: PaneTab[] = [
    'chat',
    'changes',
    ...(tabsOffered ? (['files'] as const) : []),
    ...(artifactsOffered ? (['artifacts'] as const) : []),
    ...(peopleOffered ? (['people'] as const) : []),
  ];
  const labels: Record<PaneTab, ReactNode> = {
    chat: 'Chat',
    changes:
      changedFiles > 0 ? (
        <>
          Changes <span className="fd-count">{changedFiles}</span>
        </>
      ) : (
        'Changes'
      ),
    files: 'Files',
    // The rows that the list shows (the older copies of a title fold under its row).
    artifacts:
      artifactRows !== null && artifactRows > 0 ? (
        <>
          Artifacts <span className="fd-count">{artifactRows}</span>
        </>
      ) : (
        'Artifacts'
      ),
    people:
      people.who.length >= 2 ? (
        <>
          People <span className="fd-count">{people.who.length}</span>
        </>
      ) : (
        'People'
      ),
  };
  const onTabKey = (event: KeyboardEvent<HTMLDivElement>) => {
    const at = order.indexOf(tab);
    let next: number;
    if (event.key === 'ArrowRight') next = (at + 1) % order.length;
    else if (event.key === 'ArrowLeft') next = (at - 1 + order.length) % order.length;
    else if (event.key === 'Home') next = 0;
    else if (event.key === 'End') next = order.length - 1;
    else return;
    event.preventDefault();
    const target = order[next];
    if (target === undefined) return;
    onTab(target);
    tabs.current[target]?.focus();
  };
  const panelAct = (call: Parameters<typeof controller.act>[1]) => controller.act('turns', call);
  // DL-174: the `@` menu of the composer searches the file names of the session root.
  const searchFiles =
    files !== null && filesReady
      ? async (query: string, maxHits: number) =>
          (await files.search(query, 'name', maxHits)).hits.map((h) => h.path)
      : null;
  // DL-189: the earlier history of an imported session; the deck decides who may read it.
  const historyView = useSessionHistory(
    actions?.history ?? null,
    `${key.station_id}/${key.session_id}`,
    tab === 'chat',
  );
  const historyGap =
    historyView.items.length > 0 &&
    timeline?.ready === true &&
    events.length > 0 &&
    events[0]?.event.kind !== 'session.started';
  const [fileRequest, setFileRequest] = useState<FileRequest>();
  const requests = useRef(0);
  const onOpenFile = (path: string) => {
    requests.current += 1;
    setFileRequest({ path, seq: requests.current });
    if (!sideOpen) onTab('files');
  };
  // The Files panel reads a request when it shows (child effects run first); then it is spent.
  useEffect(() => {
    if (fileRequest && (tab === 'files' || sideOpen)) setFileRequest(undefined);
  }, [fileRequest, tab, sideOpen]);
  // §4.5: a changed file in the Properties panel opens the Changes tab on that file; the tab
  // reads the request when it shows, then it is spent (as a file request).
  const [changeRequest, setChangeRequest] = useState<FileRequest>();
  const onOpenChange = (path: string) => {
    requests.current += 1;
    setChangeRequest({ path, seq: requests.current });
    onTab('changes');
  };
  useEffect(() => {
    if (changeRequest && tab === 'changes') setChangeRequest(undefined);
  }, [changeRequest, tab]);

  const artifactsValue = useMemo(
    () =>
      artifactsEnv === undefined
        ? undefined
        : {
            actions: artifactsEnv.actions,
            session: { station: session.station_id, session: s.session_id },
            open: (id: string, version: number) => {
              setOpenArtifact({ id, version });
              onTab('artifacts');
            },
            kept,
            keep: (key: string) => setKept((now) => new Set(now).add(key)),
          },
    [artifactsEnv, session.station_id, s.session_id, kept, onTab],
  );

  const pane = (
    <section className="fd-pane" aria-label="Session" data-layout={layout}>
      <SessionHeader
        session={session}
        stations={stations}
        controller={controller}
        dockRequests={dockRequests}
        actions={check.actions ?? null}
        {...(onBack ? { onBack } : {})}
        terminal={terminal}
        layout={layout}
        onNewShell={onNewShell}
        {...(sidePanel ? { filesPanel: { open: filesOpen, toggle: toggleFiles } } : {})}
        propsPanel={propsRail ? { open: propsOpen, toggle: toggleProps } : undefined}
        models={s.models ?? sessionModels(events)}
        onCheckExecution={guest === null && actions?.canAdmin === true ? check.open : undefined}
        onMoveStarted={move.reload}
        fleet={props.fleet}
        fleetControl={propsShown ? null : fleetControl.link}
        composerResume={resumeBar}
        composerModel={layout !== 'phone' && composerShown && !resumeBar}
      />
      <MoveBanner
        session={session}
        stations={stations}
        move={move.read?.move ?? null}
        owner={moveOwner}
        onChanged={move.reload}
      />
      <ArtifactsLink
        dir={s.artifacts_dir}
        events={events}
        files={filesReady ? (actions?.artifacts ?? null) : null}
      />
      {guest === null ? <LandPanel session={session} stations={stations} /> : null}
      {check.dialog && actions ? <ExecutionDialog actions={actions} {...check.dialog} /> : null}
      {fleetControl.dialog}
      {hold ? (
        <HoldNotice
          hold={hold}
          station={session.station_id}
          shells={shells}
          controller={controller}
          release={flow ? flow.release : null}
        />
      ) : null}
      <div
        ref={tabRow}
        className="fd-pane-tabs"
        role="tablist"
        aria-label="Session views"
        tabIndex={-1}
        onKeyDown={onTabKey}
      >
        {order.map((name) => (
          <button
            key={name}
            type="button"
            role="tab"
            id={`${ids}-tab-${name}`}
            aria-selected={tab === name}
            aria-controls={`${ids}-panel`}
            tabIndex={tab === name ? 0 : -1}
            ref={(el) => {
              tabs.current[name] = el;
            }}
            onClick={() => onTab(name)}
          >
            {labels[name]}
          </button>
        ))}
      </div>
      <div className="fd-pane-split">
        <div
          className={tab === 'chat' ? 'fd-pane-chat' : 'fd-pane-body'}
          role="tabpanel"
          id={`${ids}-panel`}
          aria-labelledby={`${ids}-tab-${tab}`}
          data-tab={tab}
        >
          {tab === 'chat' ? (
            <>
              {agentSub === undefined ? null : (
                <AgentHead
                  title={subagentName(agentSub)}
                  status={shownStatus(agentSub, tasks)}
                  onBack={() => onAgent(null)}
                />
              )}
              {wf === null ? null : (
                <WorkflowAgentView {...wf} onBack={() => onAgent(null)} toolbar={agentsMenu} />
              )}
              {wf !== null ? null : (
                <ChatView
                  key={agentSub?.node_id ?? 'main'}
                  items={agentSub === undefined ? tree.items : agentSub.items}
                  provider={s.provider}
                  working={
                    agentSub === undefined
                      ? s.current_run !== null
                      : isLive(shownStatus(agentSub, tasks))
                  }
                  ready={timeline?.ready === true}
                  error={timeline?.error ?? null}
                  onOpenChanges={() => onTab('changes')}
                  {...(agentSub === undefined
                    ? { history: { view: historyView, gap: historyGap } }
                    : {})}
                  loadSubagent={loadSubagent}
                  workflows={workflows}
                  tasks={tasks}
                  highlight={highlight}
                  apps={appsEnvOf(session, actions ?? null, guest)}
                  shell={
                    guest === null && actions != null
                      ? {
                          canStop: flags.drive,
                          onStop: () => controller.act('interrupt', () => actions.interrupt()),
                          canRelease: actions.canAdmin,
                          ended: flags.ended,
                          onRelease: (effectId) => actions.releaseShell(effectId),
                        }
                      : undefined
                  }
                  toolbar={agentsMenu}
                  emptyText={
                    agentSub === undefined ? undefined : 'This subagent has no messages yet.'
                  }
                  footer={
                    agentSub !== undefined &&
                    loadSubagent !== null &&
                    agentSub.tool_use_id !== null ? (
                      <SubagentWork
                        key={agentSub.node_id}
                        load={() => loadSubagent(agentSub.tool_use_id ?? '')}
                      />
                    ) : undefined
                  }
                  review={
                    reviewActions === undefined
                      ? undefined
                      : {
                          reviews: review.data?.reviews ?? [],
                          byShort: review.byShort,
                          known: review.known,
                          onOpen: openComment,
                        }
                  }
                />
              )}
              <div className="fd-pane-foot">
                {access.questions ? (
                  <ApprovalDock
                    requests={dockRequests}
                    answerable={caps?.approvals === true}
                    enabled={
                      (guest !== null ? flags.live && !flags.ended : flags.approve) && busy === null
                    }
                    busy={busy !== null}
                    from={choice.requestFrom}
                    refusals={dockRefusals(controller.refusals, dockRequests)}
                    onAnswer={answerVia(controller, actions)}
                  />
                ) : null}
                {choice.note === null ? null : (
                  <p className="fd-agent-note" role="note">
                    {choice.note}
                  </p>
                )}
                {choice.note === null && people.drive !== null && access.composer ? (
                  <MobStrip
                    view={people.drive}
                    busy={driveBusy}
                    note={driveNote}
                    onRequest={() => drive(people.requestDrive)}
                    onHandOver={(to) => drive(() => people.handOver(to))}
                    onRelease={() => drive(people.release)}
                  />
                ) : null}
                {choice.note !== null || composerHidden(s) ? null : access.composer ? (
                  <>
                    {confirmLineShown(s) ? (
                      <p className="fd-agent-note" role="note">
                        {CONFIRM_LINE}
                      </p>
                    ) : null}
                    <Composer
                      enabled={
                        (flags.drive || wakes) && busy === null && people.drive?.locked !== true
                      }
                      working={s.current_run !== null}
                      steer={caps?.steer === true}
                      nativeQueue={caps?.native_follow_up === true}
                      onSend={(kind, text, images) => {
                        if (!actions) return;
                        // B8: the first 64 chips go with the prompt; a confirmed prompt uses them up (G4).
                        const results = shellChips.toSend();
                        controller.act('send', async () => {
                          // R6.6: a hibernated session resumes first, then takes the prompt.
                          const outcome = wakes
                            ? await resumeThenSend(
                                {
                                  ...actions,
                                  resume: check.resume,
                                  send: (k, t, i) => actions.send(k, t, i, results),
                                },
                                kind,
                                text,
                                images,
                              )
                            : await actions.send(kind, text, images, results);
                          return onConfirmed(outcome, () => shellChips.consume(results));
                        });
                      }}
                      bang={bang}
                      shared={isShared(env, sessionKey(key))}
                      onShell={(command) =>
                        actions && controller.act('shell', () => actions.shellCommand(command))
                      }
                      chips={shellChips.chips}
                      onRemoveChip={shellChips.remove}
                      resumeBusy={busy !== null}
                      onResume={
                        composerResume && actions
                          ? () => controller.act('resume', check.resume)
                          : null
                      }
                      disabledReason={
                        people.drive?.locked === true
                          ? 'Request to drive to send prompts.'
                          : wakes
                            ? null
                            : disabledReason(flags, s.state === 'lost', actions)
                      }
                      phone={layout === 'phone'}
                      commands={commandList(s, events)}
                      changesRef={changesRef}
                      searchFiles={searchFiles}
                      {...(commentChoices.length > 0 ? { comments: commentChoices } : {})}
                      draftKey={`${session.station_id}/${s.session_id}`}
                      model={modelText(s.model, s.effort)}
                      callsign={s.callsign}
                      {...(guest === null &&
                      caps?.set_model === true &&
                      flags.drive &&
                      busy === null &&
                      layout !== 'phone'
                        ? {
                            modelChoice: composerModelChoice(s, events, actions, controller),
                          }
                        : {})}
                    />
                  </>
                ) : guest?.role === 'viewer' ? (
                  <p className="fd-viewer-note" role="note">
                    You are a viewer.
                  </p>
                ) : null}
              </div>
            </>
          ) : null}
          {tab === 'changes' ? (
            s.revert || turns.length > 0 || s.base || branchesOffered ? (
              <>
                <RevertStatus
                  summary={s}
                  turns={turns}
                  actions={guest === null ? actions : undefined}
                  drive={flags.drive && guest === null}
                  live={flags.live}
                  busy={busy !== null}
                  act={panelAct}
                />
                <TurnsPanel
                  summary={s}
                  turns={turns}
                  actions={actions}
                  drive={flags.drive && guest === null}
                  live={flags.live}
                  busy={busy !== null}
                  act={panelAct}
                  onOpenFile={onOpenFile}
                  fileRequest={changeRequest}
                  branchesOffered={branchesOffered}
                  projectId={session.project_id}
                  station={session.station_id}
                  review={
                    reviewActions === undefined
                      ? undefined
                      : {
                          state: review,
                          actions: reviewActions,
                          key,
                          canWrite: canWriteReview,
                          self,
                          focus,
                        }
                  }
                />
              </>
            ) : (
              <p className="fd-note">
                No finished turns yet. Changes appear here when a turn ends.
              </p>
            )
          ) : null}
          {tab === 'files' ? (
            filesReady ? (
              <FilesPanel
                files={actions?.files ?? null}
                request={fileRequest}
                root={s.cwd}
                diffRef={changesRef}
              />
            ) : (
              <p className="fd-note">
                Files are available when the board is live and you have the drive scope.
              </p>
            )
          ) : null}
          {tab === 'people' ? (
            <SideChat
              messages={people.messages}
              self={env.self}
              loading={people.loading}
              error={people.error}
              onSend={people.say}
            />
          ) : null}
          {tab === 'artifacts' && artifactsEnv !== undefined ? (
            <SessionArtifacts
              actions={artifactsEnv.actions}
              session={key}
              count={s.artifacts_count}
              open={openArtifact}
              onOpen={(id, version) => setOpenArtifact({ id, version })}
              onClose={() => setOpenArtifact(null)}
            />
          ) : null}
        </div>

        {sideOpen ? (
          <aside className="fd-pane-side" aria-label="Files panel">
            {filesReady ? (
              <FilesPanel
                files={actions?.files ?? null}
                variant="side"
                request={fileRequest}
                root={s.cwd}
                diffRef={changesRef}
              />
            ) : (
              <p className="fd-note">
                Files are available when the board is live and you have the drive scope.
              </p>
            )}
          </aside>
        ) : null}
        {propsShown ? (
          <PropertiesPanel
            session={session}
            stations={stations}
            turns={turns}
            total={total}
            agents={
              Math.max(tree.subagents.length, runningSubagents(s)) + workflowAgentTotal(workflows)
            }
            commands={commandTasks(tasks).length}
            runningAgents={running}
            onOpenChange={onOpenChange}
            onOpenAll={() => onTab('changes')}
            freshness={freshness}
            fleet={fleetControl.link?.()}
            mode={
              guest === null && caps?.set_mode === true && caps.modes.length > 0
                ? {
                    modes: caps.modes,
                    enabled: flags.drive && busy === null,
                    onSet: (m) => actions && controller.act('mode', () => actions.setMode(m)),
                  }
                : undefined
            }
          />
        ) : null}
      </div>
    </section>
  );
  return artifactsValue === undefined ? (
    pane
  ) : (
    <ArtifactsProvider value={artifactsValue}>{pane}</ArtifactsProvider>
  );
}
