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
import { ChatView } from '../chat/ChatView';
import { useSessionHistory } from '../chat/history';
import type { DeckSession, StationView } from '../gen';
import type { Freshness, Timeline } from '../runtime/state';
import { usePeople } from '../sharing/context';
import { usePresenceHeartbeat, useSessionPeople } from '../sharing/hooks';
import { roleAccess } from '../sharing/member';
import { useReviewActions } from '../review/context';
import { commentOptions } from '../review/hashMenu';
import { useReview } from '../review/useReview';
import { MobStrip } from '../sharing/MobStrip';
import { SideChat } from '../sharing/SideChat';
import type { ShellActions } from '../shell/shell';
import { releaseHold, type Watch } from '../terminal/handover';
import type { TerminalStore } from '../terminal/store';
import { ApprovalDock } from './ApprovalDock';
import { hasChanges } from './changes';
import type { SessionActions } from './commands';
import { isHibernated, resumeThenSend } from '../views/hibernate';
import { Composer } from './Composer';
import { useSessionController } from './controller';
import { FilesPanel } from './FilesPanel';
import { handoverFlow } from './handoverFlow';
import { ExecutionDialog } from './ExecutionDialog';
import { FleetControlChip } from './FleetControlChip';
import { SessionHeader } from './SessionHeader';
import { buildTimeline, sessionModels, slashCommands } from './timeline';
import { RevertStatus, TurnsPanel } from './TurnsPanel';
import { finalizedTurns } from './workspace';

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
  readonly onBack?: () => void;
  /** The phone shows the terminal screen. */
  readonly onTerminalOpened?: () => void;
  /** Opens a shell in the workspace; the session menu offers it when given. */
  readonly onNewShell?: (() => void) | undefined;
};

const noop = () => {};

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
  const turns = finalizedTurns(events);
  const filesOffered = actions?.files != null;
  const wakes = isHibernated(session) && flags.canResume && guest === null;
  const wide = useWide();
  const [filesOpen, toggleFiles] = useFilesOpen();
  const sidePanel = layout === 'desktop' && wide && filesOffered;
  const sideOpen = sidePanel && filesOpen;
  const tabsOffered = filesOffered && !sideOpen;
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
  const hold = s.terminal ?? null;
  const [confirmRelease, setConfirmRelease] = useState(false);
  const [checking, setChecking] = useState(false);
  const ids = useId();
  const tabs = useRef<Record<string, HTMLButtonElement | null>>({});
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
  const [fileRequest, setFileRequest] = useState<{ readonly path: string; readonly seq: number }>();
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
        models={s.models ?? sessionModels(events)}
        onCheckExecution={
          guest === null && actions?.canAdmin === true ? () => setChecking(true) : undefined
        }
      />
      {guest === null && actions?.canAdmin === true ? <FleetControlChip actions={actions} /> : null}
      {checking && actions ? (
        <ExecutionDialog actions={actions} onClose={() => setChecking(false)} />
      ) : null}
      {hold ? (
        <div className="fd-pane-notice" role="note">
          <p>
            A terminal holds this session (shell {hold.shell_id}).
            {hold.reason ? ` ${hold.reason}` : ''}
          </p>
          {shells ? (
            confirmRelease ? (
              <div className="fd-pane-confirm" role="group" aria-label="Release the hold">
                <p>Only do this when no terminal process uses this session.</p>
                <div className="fd-pane-row">
                  <button
                    type="button"
                    className="fd-sm"
                    disabled={busy !== null}
                    onClick={() => {
                      setConfirmRelease(false);
                      controller.act('release', () =>
                        flow
                          ? flow.release(hold.shell_id)
                          : releaseHold(() => shells.release(session.station_id, hold.shell_id)),
                      );
                    }}
                  >
                    Release the hold
                  </button>
                  <button type="button" className="fd-sm" onClick={() => setConfirmRelease(false)}>
                    Cancel
                  </button>
                </div>
              </div>
            ) : (
              <button
                type="button"
                className="fd-sm"
                disabled={busy !== null}
                onClick={() => setConfirmRelease(true)}
              >
                Release the hold
              </button>
            )
          ) : null}
        </div>
      ) : null}
      <div
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
              <ChatView
                items={tree.items}
                provider={s.provider}
                working={s.current_run !== null}
                ready={timeline?.ready === true}
                error={timeline?.error ?? null}
                onOpenChanges={() => onTab('changes')}
                history={{ view: historyView, gap: historyGap }}
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
                {people.drive !== null && access.composer ? (
                  <MobStrip
                    view={people.drive}
                    busy={driveBusy}
                    note={driveNote}
                    onRequest={() => drive(people.requestDrive)}
                    onHandOver={(to) => drive(() => people.handOver(to))}
                    onRelease={() => drive(people.release)}
                  />
                ) : null}
                {access.composer ? (
                  <Composer
                    enabled={
                      (flags.drive || wakes) && busy === null && people.drive?.locked !== true
                    }
                    working={s.current_run !== null}
                    steer={caps?.steer === true}
                    nativeQueue={caps?.native_follow_up === true}
                    onSend={(kind, text, images) =>
                      actions &&
                      controller.act('send', () =>
                        // R6.6: a hibernated session resumes first, then takes the prompt.
                        wakes
                          ? resumeThenSend(actions, kind, text, images)
                          : actions.send(kind, text, images),
                      )
                    }
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
                  />
                ) : guest?.role === 'viewer' ? (
                  <p className="fd-viewer-note" role="note">
                    You are a viewer.
                  </p>
                ) : null}
              </div>
            </>
          ) : null}
          {tab === 'changes' ? (
            s.revert || turns.length > 0 || s.base ? (
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
              <FilesPanel files={actions?.files ?? null} request={fileRequest} root={s.cwd} />
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
              />
            ) : (
              <p className="fd-note">
                Files are available when the board is live and you have the drive scope.
              </p>
            )}
          </aside>
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
