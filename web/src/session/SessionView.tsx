// DL-140: the session pane: header, the notice of a terminal hold, the tabs
// (Chat | Changes | Files), the chat with the approval dock and the composer.
import { type KeyboardEvent, useId, useMemo, useRef, useState } from 'react';
import { ChatView } from '../chat/ChatView';
import type { DeckSession, StationView } from '../gen';
import type { Freshness, Timeline } from '../runtime/state';
import type { ShellActions } from '../shell/shell';
import { releaseHold, type Watch } from '../terminal/handover';
import type { TerminalStore } from '../terminal/store';
import { ApprovalDock } from './ApprovalDock';
import type { SessionActions } from './commands';
import { Composer } from './Composer';
import { useSessionController } from './controller';
import { FilesPanel } from './FilesPanel';
import { handoverFlow } from './handoverFlow';
import { SessionHeader } from './SessionHeader';
import { buildTimeline, slashCommands } from './timeline';
import { RevertStatus, TurnsPanel } from './TurnsPanel';
import { finalizedTurns } from './workspace';

export type PaneTab = 'chat' | 'changes' | 'files';

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
};

const noop = () => {};

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
  const { layout = 'desktop', onTab = noop, onBack, onTerminalOpened } = props;
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
  const tab: PaneTab = props.tab === 'files' && !filesOffered ? 'chat' : (props.tab ?? 'chat');
  const flow =
    actions && shells && terminals && watch
      ? handoverFlow({ key, title: s.title ?? s.session_id, watch, actions, shells, terminals })
      : null;
  const terminal = flow
    ? {
        available: true,
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
  const order: PaneTab[] = filesOffered ? ['chat', 'changes', 'files'] : ['chat', 'changes'];
  const labels: Record<PaneTab, string> = {
    chat: 'Chat',
    changes: turns.length > 0 ? `Changes · ${turns.length}` : 'Changes',
    files: 'Files',
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

  return (
    <section className="fd-pane" aria-label="Session" data-layout={layout}>
      <SessionHeader
        session={session}
        stations={stations}
        controller={controller}
        actions={actions ?? null}
        {...(onBack ? { onBack } : {})}
        terminal={terminal}
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
                  <button type="button" onClick={() => setConfirmRelease(false)}>
                    Cancel
                  </button>
                </div>
              </div>
            ) : (
              <button
                type="button"
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
                  actions && controller.act('answer', () => actions.answer(request, option, reply))
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
          s.revert || turns.length > 0 ? (
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
              />
            </>
          ) : (
            <p className="fd-note">No finished turns yet. Changes appear here when a turn ends.</p>
          )
        ) : null}
        {tab === 'files' ? (
          filesReady ? (
            <FilesPanel files={actions?.files ?? null} />
          ) : (
            <p className="fd-note">
              Files are available when the board is live and you have the drive scope.
            </p>
          )
        ) : null}
      </div>
    </section>
  );
}
