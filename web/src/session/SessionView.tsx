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
import type { DeckSession, StationView } from '../gen';
import type { Freshness, Timeline } from '../runtime/state';
import type { ShellActions } from '../shell/shell';
import { releaseHold, type Watch } from '../terminal/handover';
import type { TerminalStore } from '../terminal/store';
import { ApprovalDock } from './ApprovalDock';
import { hasChanges } from './changes';
import type { SessionActions } from './commands';
import { Composer } from './Composer';
import { useSessionController } from './controller';
import { FilesPanel } from './FilesPanel';
import { handoverFlow } from './handoverFlow';
import { SessionHeader } from './SessionHeader';
import { buildTimeline, sessionModels, slashCommands } from './timeline';
import { RevertStatus, TurnsPanel } from './TurnsPanel';
import { finalizedTurns } from './workspace';

export type PaneTab = 'chat' | 'changes' | 'files' | 'artifacts';

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

/** The open state of the Files side panel, kept in `localStorage`; unreadable means open. */
function useFilesOpen(): readonly [boolean, () => void] {
  const [open, setOpen] = useState(() => {
    try {
      return localStorage.getItem(PANEL_KEY) !== '0';
    } catch {
      return true;
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
  const events = timeline?.events ?? [];
  const tree = buildTimeline(events);
  const turns = finalizedTurns(events);
  const filesOffered = actions?.files != null;
  const wide = useWide();
  const [filesOpen, toggleFiles] = useFilesOpen();
  const sidePanel = layout === 'desktop' && wide && filesOffered;
  const sideOpen = sidePanel && filesOpen;
  const tabsOffered = filesOffered && !sideOpen;
  const artifactsEnv = useArtifacts();
  const artifactsOffered = artifactsEnv !== undefined;
  const wanted: PaneTab = props.tab ?? 'chat';
  const tab: PaneTab =
    (wanted === 'files' && !tabsOffered) || (wanted === 'artifacts' && !artifactsOffered)
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
  const ids = useId();
  const tabs = useRef<Record<string, HTMLButtonElement | null>>({});
  const changed = turns.filter((t) => !t.reverted && hasChanges(t.diff)).length;
  const order: PaneTab[] = [
    'chat',
    'changes',
    ...(tabsOffered ? (['files'] as const) : []),
    ...(artifactsOffered ? (['artifacts'] as const) : []),
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
  const filesReady = flags.live && actions?.canDrive === true;
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
      />
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
                working={s.current_run !== null}
                ready={timeline?.ready === true}
                error={timeline?.error ?? null}
                onOpenChanges={() => onTab('changes')}
              />
              <div className="fd-pane-foot">
                <ApprovalDock
                  requests={s.open_requests}
                  answerable={caps?.approvals === true}
                  enabled={flags.approve && busy === null}
                  busy={busy !== null}
                  onAnswer={(request, option, reply) =>
                    actions &&
                    controller.act('answer', () => actions.answer(request, option, reply))
                  }
                />
                <Composer
                  enabled={flags.drive && busy === null}
                  working={s.current_run !== null}
                  steer={caps?.steer === true}
                  nativeQueue={caps?.native_follow_up === true}
                  onSend={(kind, text, images) =>
                    actions && controller.act('send', () => actions.send(kind, text, images))
                  }
                  disabledReason={disabledReason(flags, actions)}
                  slashCommands={slashCommands(events)}
                  draftKey={`${session.station_id}/${s.session_id}`}
                />
              </div>
            </>
          ) : null}
          {tab === 'changes' ? (
            s.revert || turns.length > 0 || s.base ? (
              <>
                <RevertStatus
                  summary={s}
                  turns={turns}
                  actions={actions}
                  drive={flags.drive}
                  live={flags.live}
                  busy={busy !== null}
                  act={panelAct}
                />
                <TurnsPanel
                  summary={s}
                  turns={turns}
                  actions={actions}
                  drive={flags.drive}
                  live={flags.live}
                  busy={busy !== null}
                  act={panelAct}
                  onOpenFile={onOpenFile}
                  station={session.station_id}
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
