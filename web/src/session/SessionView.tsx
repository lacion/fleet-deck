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
  useSyncExternalStore,
} from 'react';
import { ArtifactsProvider, useArtifacts } from '../artifacts/context';
import { SessionArtifacts } from '../artifacts/SessionArtifacts';
import { type AgentChoice, AgentHead, AgentsMenu } from '../chat/AgentsMenu';
import { ChatView } from '../chat/ChatView';
import { SubagentWork } from '../chat/SubagentWork';
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
import { ApprovalDock } from './ApprovalDock';
import { hasChanges } from './changes';
import type { SessionActions } from './commands';
import { isHibernated, resumeThenSend } from '../views/hibernate';
import { Composer } from './Composer';
import { useSessionController } from './controller';
import { ArtifactsLink } from './ArtifactsLink';
import { FilesPanel } from './FilesPanel';
import { handoverFlow } from './handoverFlow';
import { HoldNotice } from './HoldNotice';
import { ExecutionDialog } from './ExecutionDialog';
import { FleetControlChip } from './FleetControlChip';
import { LandPanel } from './LandPanel';
import { MoveBanner } from './MoveBanner';
import { useMove } from './MoveDialog';
import { CONFIRM_LINE, composerHidden, confirmLineShown } from './moveModel';
import { PropertiesPanel, usePropsOpen } from './PropertiesPanel';
import { SessionHeader } from './SessionHeader';
import { useTabReserve } from './tabReserve';
import { hasAgents, isLive, shownStatus, taskStates, toolTitles } from './tasks';
import { buildTimeline, sessionModels, slashCommands } from './timeline';
import { type FileRequest, RevertStatus, TurnsPanel } from './TurnsPanel';
import { finalizedTurns } from './workspace';
import { shortModel } from '../ui/format';
import { sessionRef } from '../ui/localSet';

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

const HIGHLIGHT_HOLD_MS = 2500;

const WIDE = '(min-width: 1280px)';
const PANEL_KEY = 'fd-files-panel';

/** True while the window is at least 1280 px wide (the Files side panel needs it). */
function useWide(): boolean {
  return useSyncExternalStore(
    (notify) => {
      if (typeof window.matchMedia !== 'function') return () => {};
      const query = window.matchMedia(WIDE);
      query.addEventListener('change', notify);
      return () => query.removeEventListener('change', notify);
    },
    () => typeof window.matchMedia !== 'function' || window.matchMedia(WIDE).matches,
    () => true,
  );
}

/** The stored open state of the Files side panel: only a stored `1` opens it; unreadable means closed. */
export function readFilesOpen(storage: Pick<Storage, 'getItem'>): boolean {
  try {
    return storage.getItem(PANEL_KEY) === '1';
  } catch {
    return false;
  }
}

/** The open state of the Files side panel, kept in `localStorage`; the default is closed. */
function useFilesOpen(): readonly [boolean, () => void] {
  const [open, setOpen] = useState(() => {
    try {
      return readFilesOpen(localStorage);
    } catch {
      return false;
    }
  });
  const toggle = useCallback(() => {
    setOpen((now) => {
      try {
        localStorage.setItem(PANEL_KEY, now ? '0' : '1');
      } catch {
        // the choice stays in memory
      }
      return !now;
    });
  }, []);
  return [open, toggle];
}

function disabledReason(
  flags: { live: boolean; held: boolean; ended: boolean },
  actions: SessionActions | undefined,
): string | null {
  if (!flags.live) return 'Controls are off until the board and the station are live.';
  if (flags.held)
    return 'A terminal holds this session. Return it to board control to send prompts.';
  if (flags.ended) return 'The session ended. Resume it to send prompts.';
  if (actions?.canDrive !== true) return 'Sending prompts needs the drive scope.';
  return null;
}

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
  // B6: a chosen background command: its card is scrolled to and marked; the mark is not kept.
  const [highlight, setHighlight] = useState<{ node_id: string; nonce: number } | null>(null);
  useEffect(() => {
    if (highlight === null) return;
    const timer = setTimeout(() => setHighlight(null), HIGHLIGHT_HOLD_MS);
    return () => clearTimeout(timer);
  }, [highlight]);
  // Only the node id of a subagent selects an agent view; any other value shows the main chat.
  const loadSubagent = guest === null ? (actions?.subagent ?? null) : null;
  const titles = toolTitles(tree.items);
  // B8: the Claude shell results that the next prompt carries.
  const shellChips = useShellChips(sessionKey(key), tree.items, s.provider);
  const agentSub =
    props.tab === 'chat' || props.tab === undefined
      ? tree.subagents.find((sub) => sub.node_id === props.agent)
      : undefined;
  const chooseAgent = (choice: AgentChoice) => {
    if (choice.kind === 'task') {
      onAgent(null);
      setHighlight({ node_id: choice.node_id, nonce: Date.now() });
      return;
    }
    setHighlight(null);
    onAgent(choice.kind === 'main' ? null : choice.node_id);
  };
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
  const [checking, setChecking] = useState(false);
  // FG-9: the newest move of the session (owner only) feeds the move banner.
  const integrations = useIntegrations();
  const moveOwner = guest === null && integrations?.actions.canAdmin === true;
  const move = useMove(session, moveOwner);
  const ids = useId();
  const tabs = useRef<Record<string, HTMLButtonElement | null>>({});
  // The composer model chip opens the header menu on its model picker (the header sets it).
  const openModel = useRef<(() => void) | null>(null);
  const tabRow = useRef<HTMLDivElement>(null);
  useTabReserve(tabRow);
  const changed = turns.filter((t) => !t.reverted && hasChanges(t.diff)).length;
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
      changed > 0 ? (
        <>
          Changes <span className="fd-count">{changed}</span>
        </>
      ) : (
        'Changes'
      ),
    files: 'Files',
    artifacts:
      s.artifacts_count > 0 ? (
        <>
          Artifacts <span className="fd-count">{s.artifacts_count}</span>
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
  // A viewer has no drive scope but sees the files of the shared session.
  const filesReady = flags.live && (actions?.canDrive === true || guest !== null);
  const changesRef = sessionRef(session.station_id, s.session_id);
  const files = actions?.files ?? null;
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
        actions={actions ?? null}
        {...(onBack ? { onBack } : {})}
        terminal={terminal}
        layout={layout}
        onNewShell={onNewShell}
        {...(sidePanel ? { filesPanel: { open: filesOpen, toggle: toggleFiles } } : {})}
        propsPanel={propsRail ? { open: propsOpen, toggle: toggleProps } : undefined}
        models={s.models ?? sessionModels(events)}
        onCheckExecution={
          guest === null && actions?.canAdmin === true ? () => setChecking(true) : undefined
        }
        onMoveStarted={move.reload}
        fleet={props.fleet}
        openModel={openModel}
      />
      <MoveBanner
        session={session}
        stations={stations}
        move={move.read?.move ?? null}
        owner={moveOwner}
        onChanged={move.reload}
      />
      {guest === null && actions?.canAdmin === true ? <FleetControlChip actions={actions} /> : null}
      <ArtifactsLink
        dir={s.artifacts_dir}
        events={events}
        files={filesReady ? (actions?.artifacts ?? null) : null}
      />
      {guest === null ? <LandPanel session={session} stations={stations} /> : null}
      {checking && actions ? (
        <ExecutionDialog actions={actions} onClose={() => setChecking(false)} />
      ) : null}
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
                  title={agentSub.title}
                  status={shownStatus(agentSub, tasks)}
                  onBack={() => onAgent(null)}
                />
              )}
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
                tasks={tasks}
                highlight={highlight}
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
                toolbar={
                  hasAgents(tree.subagents, tasks) ? (
                    <AgentsMenu
                      subagents={tree.subagents}
                      tasks={tasks}
                      toolTitles={titles}
                      agent={agentSub?.node_id ?? null}
                      onChoose={chooseAgent}
                    />
                  ) : undefined
                }
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
              <div className="fd-pane-foot">
                {access.questions ? (
                  <ApprovalDock
                    requests={
                      access.approvals
                        ? s.open_requests
                        : s.open_requests.filter((r) => r.request_kind === 'question')
                    }
                    answerable={caps?.approvals === true}
                    enabled={
                      (guest !== null ? flags.live && !flags.ended : flags.approve) && busy === null
                    }
                    busy={busy !== null}
                    onAnswer={(request, option, reply) =>
                      actions &&
                      controller.act('answer', () => actions.answer(request, option, reply))
                    }
                  />
                ) : null}
                {agentSub !== undefined ? (
                  <p className="fd-agent-note" role="note">
                    A subagent takes no input. Write in the main chat.
                  </p>
                ) : null}
                {agentSub === undefined && people.drive !== null && access.composer ? (
                  <MobStrip
                    view={people.drive}
                    busy={driveBusy}
                    note={driveNote}
                    onRequest={() => drive(people.requestDrive)}
                    onHandOver={(to) => drive(() => people.handOver(to))}
                    onRelease={() => drive(people.release)}
                  />
                ) : null}
                {agentSub !== undefined || composerHidden(s) ? null : access.composer ? (
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
                        // B8: the first 64 chips go with the prompt; a sent prompt uses them up.
                        const results = shellChips.toSend();
                        controller.act('send', async () => {
                          // R6.6: a hibernated session resumes first, then takes the prompt.
                          const outcome = wakes
                            ? await resumeThenSend(
                                {
                                  ...actions,
                                  send: (k, t, i) => actions.send(k, t, i, results),
                                },
                                kind,
                                text,
                                images,
                              )
                            : await actions.send(kind, text, images, results);
                          if (outcome.ok) shellChips.consume(results);
                          return outcome;
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
                        flags.canResume &&
                        !wakes &&
                        guest === null &&
                        actions !== null &&
                        actions !== undefined
                          ? () => controller.act('resume', () => actions.resume())
                          : null
                      }
                      disabledReason={
                        people.drive?.locked === true
                          ? 'Request to drive to send prompts.'
                          : wakes
                            ? null
                            : disabledReason(flags, actions)
                      }
                      slashCommands={s.slash_commands ?? slashCommands(events)}
                      skills={s.skills ?? []}
                      searchFiles={searchFiles}
                      {...(commentChoices.length > 0 ? { comments: commentChoices } : {})}
                      draftKey={`${session.station_id}/${s.session_id}`}
                      model={shortModel(s.model)}
                      callsign={s.callsign}
                      {...(guest === null &&
                      caps?.set_model === true &&
                      flags.drive &&
                      busy === null
                        ? { onModel: () => openModel.current?.() }
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
        {propsRail && propsOpen ? (
          <PropertiesPanel
            session={session}
            stations={stations}
            turns={turns}
            subagents={tree.subagents.length}
            onOpenChange={onOpenChange}
            files={filesReady ? files : null}
            freshness={freshness}
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
