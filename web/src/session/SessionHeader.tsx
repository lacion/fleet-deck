// DL-140: the header of a session: title, status, provider, model, where it
// runs, and the actions (interrupt, resume, terminal) with one overflow menu (DL-141).
import { type ReactNode, useState } from 'react';
import { formatHash } from '../app/routes';
import { stationName } from '../board/fleet';
import type { DeckSession, ModelChoice, StationView } from '../gen';
import { GeneratedMark } from '../ui/generated';
import { Menu } from '../ui/Menu';
import {
  Archive,
  ArchiveRestore,
  Check,
  ChevronLeft,
  Ellipsis,
  FileText,
  FolderGit2,
  GitCompare,
  Icon,
  OctagonAlert,
  PanelRight,
  Pencil,
  Play,
  Power,
  Search,
  Server,
  Send,
  Share2,
  ShieldCheck,
  CircleStop,
  SquareTerminal,
  Star,
  TriangleAlert,
  Undo2,
  X,
} from '../ui/icons';
import { canShare, usePeople } from '../sharing/context';
import { Avatars } from '../sharing/Avatars';
import { roleAccess } from '../sharing/member';
import { openShare } from '../sharing/shareStore';
import { archived } from '../ui/archive';
import { sessionKey } from '../runtime/state';
import { priority, sessionRef } from '../ui/localSet';
import { useNow } from '../ui/useNow';
import type { Outcome, SessionActions } from './commands';
import type { Controller } from './controller';
import { useIntegrations, useProjects } from '../integrations/context';
import { GitPullRequest } from '../integrations/icons';
import { projectOfSession } from '../integrations/model';
import { OpenPrDialog } from '../integrations/OpenPrDialog';
import type { FleetActions } from '../fleet/fleet';
import { SessionAccessDialog } from '../fleet/SessionAccess';
import { Badges, Details, Facts, Place, StatusFact, Who } from './HeaderBadges';
import { MoveDialog } from './MoveDialog';
import { MoveToProject } from './MoveToProject';
import { RenameForm } from './RenameForm';
import { SendToWall } from '../wall/SendToWall';
import { FolderTree } from './icons';
import { KillConfirm, menuGroups, OpenConfirm, ShellsOffNote } from './MenuParts';
import { AutoContinueSwitch, ModeLadder, ModelPicker } from './SessionSettings';

export { CopyButton } from './CopyButton';

type TerminalProps = {
  readonly available: boolean;
  readonly onOpen: () => void;
  readonly onReturn: () => void;
};

type Props = {
  readonly session: DeckSession;
  readonly stations: readonly StationView[];
  readonly controller: Controller;
  readonly actions: SessionActions | null;
  readonly onBack?: () => void;
  readonly terminal: TerminalProps | null;
  readonly layout?: 'desktop' | 'tablet' | 'phone';
  /** Opens a shell in the workspace; the menu offers it when given. */
  readonly onNewShell?: (() => void) | undefined;
  /** The Files side panel: the header shows its toggle when given. */
  readonly filesPanel?: { readonly open: boolean; readonly toggle: () => void };
  /** The properties panel: the header shows its toggle when given. */
  readonly propsPanel?: { readonly open: boolean; readonly toggle: () => void } | undefined;
  /** The models that the provider harness reports (`session.models`): the phone menu lists them. */
  readonly models?: readonly ModelChoice[] | null;
  /** Opens the check of the previous run (DL-182); the menu offers it when given (owner only). */
  readonly onCheckExecution?: (() => void) | undefined;
  /** FG-9: a move started from the menu; the session view reads the move again. */
  readonly onMoveStarted?: (() => void) | undefined;
  /** The fleet calls: the menu offers "Fleet access…" when given (admin only). */
  readonly fleet?: FleetActions | undefined;
  /** The fleet-control fact for the details of the menu (its argument closes the menu); `null`: none. */
  readonly fleetControl?: ((close: () => void) => ReactNode) | null | undefined;
  /** The composer shows its Resume bar: the header then has no Resume (one primary). */
  readonly composerResume?: boolean | undefined;
};

/** The status update that "Ask for a status update" sends. */
const NUDGE =
  'Give a short status update: what you finished, what you do now, and what blocks you.';

/** "retry in 3 min" (rounded up), or "retry in 40 s" under a minute; `null` when the time is past. */
function retryText(retryAt: number | null, now: number): string | null {
  if (retryAt === null || retryAt <= now) return null;
  const ms = retryAt - now;
  return ms < 60_000
    ? `retry in ${Math.ceil(ms / 1000)} s`
    : `retry in ${Math.ceil(ms / 60_000)} min`;
}

export function SessionHeader({
  session,
  stations,
  controller,
  actions,
  onBack,
  terminal,
  layout = 'desktop',
  onNewShell,
  filesPanel,
  propsPanel,
  models = null,
  onCheckExecution,
  onMoveStarted,
  fleet,
  fleetControl = null,
  composerResume = false,
}: Props) {
  const s = session.summary;
  const caps = s.capabilities;
  const { flags, busy } = controller;
  const phone = layout === 'phone';
  // R11 (DL-166): a member never renames, stops, resumes, sets the mode or the model, or opens a
  // shell; the owner can share the session.
  const people = usePeople();
  const guest = people.member !== null;
  const mayPrompt = people.member === null || roleAccess(people.member.role).composer;
  const [renaming, setRenaming] = useState(false);
  const [confirm, setConfirm] = useState<'kill' | 'open' | null>(null);
  const [termConfirm, setTermConfirm] = useState(false);
  // R3.9 (DL-164): a pull request from the session's branch, for a project with a forge.
  const prProject = projectOfSession(useProjects(), session.station_id, s);
  const [prOpen, setPrOpen] = useState(false);
  // DL-176: the owner moves a session to a project; the deck needs the drive scope.
  const canMove = useIntegrations()?.actions.canDrive === true && !guest;
  const [moving, setMoving] = useState(false);
  const syncProject = useProjects().find((p) => p.project_id === session.project_id);
  // FG-9 (§12.8): "Move to station…" for the owner, in a project with sync on.
  const canMoveStation =
    !guest &&
    actions?.canAdmin === true &&
    syncProject?.git.sync === true &&
    s.moved_to === undefined;
  const [movingStation, setMovingStation] = useState(false);
  const canAccess = fleet !== undefined && !guest && actions?.canAdmin === true;
  const [accessOpen, setAccessOpen] = useState(false);
  const run = (name: string, call: (a: SessionActions) => Promise<Outcome>) => {
    if (actions) controller.act(name, () => call(actions));
  };
  const title = s.title ?? s.callsign ?? s.session_id;
  const canRename = !guest && flags.live && actions?.canDrive === true && busy === null;
  const canTerminalOpen =
    terminal?.available === true &&
    caps?.resume === true &&
    flags.live &&
    actions?.canDrive === true;
  const settingsEnabled = flags.drive && busy === null;
  const hasModes = !guest && caps?.set_mode === true && caps.modes.length > 0;
  const hasModel = !guest && caps?.set_model === true;
  const hasAuto = !guest && caps?.resume === true;
  // The Properties panel shows the facts and the mode while it is open; else the menu shows them.
  const panelOpen = propsPanel?.open === true;
  const place = (
    <Place
      station={stationName(session.station_id, stations)}
      branch={s.branch}
      cwd={s.cwd}
      {...(phone ? {} : { session })}
    />
  );
  const outcome = controller.outcome;
  const now = useNow(undefined, 30000);
  const ref = sessionRef(session.station_id, s.session_id);
  const starred = priority.use().has(ref);
  const isArchived = archived.use().has(ref);
  const canArchive = session.status === 'ended' || session.status === 'lost';
  const retry = retryText(s.last_error?.retry_at ?? null, now);
  const showError =
    s.last_error != null && (s.state === 'exited' || s.state === 'lost' || s.current_run === null);

  /** The confirm of "Open in terminal", inside a menu. */
  const openConfirm = (
    close: () => void,
    open: () => void,
    reset: () => void = () => setConfirm(null),
  ) => (
    <OpenConfirm
      ended={flags.ended}
      busy={busy !== null}
      onOpen={() => {
        reset();
        close();
        open();
      }}
      onCancel={() => {
        reset();
        close();
      }}
    />
  );

  /**
   * The item groups of the session menu (a rule between two groups that have items): the session
   * itself, the work, the settings that the screen shows nowhere else, then the end of the session.
   */
  const menuItems = (close: () => void) => {
    const session_: ReactNode[] = [
      guest ? null : (
        <button
          key="rename"
          type="button"
          disabled={!canRename}
          onClick={() => {
            close();
            setRenaming(true);
          }}
        >
          <Icon of={Pencil} /> Rename
        </button>
      ),
      <button key="star" type="button" onClick={() => priority.toggle(ref)}>
        <Icon of={Star} /> {starred ? 'Unstar' : 'Star'}
      </button>,
      canMove ? (
        <button
          key="move"
          type="button"
          onClick={() => {
            close();
            setMoving(true);
          }}
        >
          <Icon of={FolderGit2} /> Move to project…
        </button>
      ) : null,
      canMoveStation ? (
        <button
          key="move-station"
          type="button"
          onClick={() => {
            close();
            setMovingStation(true);
          }}
        >
          <Icon of={Server} /> Move to station…
        </button>
      ) : null,
      canShare(people) ? (
        <button
          key="share"
          type="button"
          onClick={() => {
            close();
            openShare({
              kind: 'session',
              target: `${session.station_id}/${s.session_id}`,
              label: title,
            });
          }}
        >
          <Icon of={Share2} /> Share session…
        </button>
      ) : null,
      guest ? null : (
        <SendToWall
          key="wall"
          target={{ kind: 'session', station_id: session.station_id, session_id: s.session_id }}
          onOpen={close}
        />
      ),
      canAccess ? (
        <button
          key="access"
          type="button"
          onClick={() => {
            close();
            setAccessOpen(true);
          }}
        >
          <Icon of={ShieldCheck} /> Fleet access…
        </button>
      ) : null,
    ];
    const work: ReactNode[] = [
      flags.drive && mayPrompt ? (
        <button
          key="nudge"
          type="button"
          disabled={busy !== null}
          onClick={() => {
            close();
            run('nudge', (a) => a.send(s.current_run === null ? 'prompt' : 'follow_up', NUDGE));
          }}
        >
          <Icon of={Send} /> Ask for a status update
        </button>
      ) : null,
      flags.live && actions?.canDrive === true ? (
        <button
          key="summarize"
          type="button"
          disabled={busy !== null}
          onClick={() => {
            close();
            run('summarize', (a) => a.summarize());
          }}
        >
          <Icon of={FileText} /> Summarize
        </button>
      ) : null,
      !guest && onCheckExecution !== undefined ? (
        <button
          key="check"
          type="button"
          onClick={() => {
            close();
            onCheckExecution();
          }}
        >
          <Icon of={Search} /> Check the previous run
        </button>
      ) : null,
      s.batch !== undefined && s.batch.size >= 2 && !guest ? (
        <button
          key="compare"
          type="button"
          onClick={() => {
            close();
            window.location.hash = formatHash({ name: 'compare', batch: s.batch?.batch_id ?? '' });
          }}
        >
          <Icon of={GitCompare} /> Compare batch
        </button>
      ) : null,
      prProject !== undefined && !guest && actions?.canDrive === true && s.branch ? (
        <button
          key="pr"
          type="button"
          onClick={() => {
            close();
            setPrOpen(true);
          }}
        >
          <Icon of={GitPullRequest} /> Open a pull request…
        </button>
      ) : null,
      onNewShell && !guest ? (
        <button
          key="shell"
          type="button"
          onClick={() => {
            close();
            onNewShell();
          }}
        >
          <Icon of={SquareTerminal} /> Open shell in workspace
        </button>
      ) : null,
      // The phone has no Terminal menu: the handover and its way back are items of this menu.
      !phone || guest || !terminal ? null : flags.held ? (
        <button
          key="return"
          type="button"
          disabled={!flags.live || actions?.canDrive !== true || busy !== null}
          onClick={() => {
            close();
            terminal.onReturn();
          }}
        >
          <Icon of={Undo2} /> Return to board control
        </button>
      ) : canTerminalOpen ? (
        <button
          key="open-terminal"
          type="button"
          disabled={busy !== null}
          onClick={() => setConfirm('open')}
        >
          <Icon of={SquareTerminal} /> Open in terminal…
        </button>
      ) : null,
      terminal !== null && !terminal.available && !guest ? (
        <ShellsOffNote key="shells-off" station={session.station_id} onOpen={close} />
      ) : null,
    ];
    const settings: ReactNode[] = [
      // The Properties panel holds the mode while it shows; else this menu does.
      hasModes && caps && !panelOpen ? (
        <ModeLadder
          key="mode"
          modes={caps.modes}
          current={s.mode}
          enabled={settingsEnabled}
          onSet={(m) => run('mode', (a) => a.setMode(m))}
        />
      ) : null,
      // The composer model chip holds the model; the phone hides the chip, so its menu does.
      hasModel && phone ? (
        <ModelPicker
          key="model"
          current={s.model}
          models={models}
          enabled={settingsEnabled}
          onSet={(m) => run('model', (a) => a.setModel(m))}
          onDone={close}
        />
      ) : null,
      hasAuto ? (
        <AutoContinueSwitch
          key="auto"
          checked={s.auto_continue}
          enabled={flags.live && actions?.canDrive === true && busy === null}
          onSet={(on) => run('auto-continue', (a) => a.setAutoContinue(on))}
        />
      ) : null,
    ];
    const end: ReactNode[] = [
      flags.canStop && !guest ? (
        <button
          key="stop"
          type="button"
          disabled={busy !== null}
          onClick={() => {
            close();
            run('stop', (a) => a.stop('graceful'));
          }}
        >
          <Icon of={Power} /> Stop
        </button>
      ) : null,
      flags.canStop && !guest ? (
        <button
          key="kill"
          type="button"
          className="fd-danger"
          disabled={busy !== null}
          onClick={() => setConfirm('kill')}
        >
          <Icon of={OctagonAlert} /> Force stop…
        </button>
      ) : null,
      canArchive ? (
        <button key="archive" type="button" onClick={() => archived.toggle(ref)}>
          <Icon of={isArchived ? ArchiveRestore : Archive} /> {isArchived ? 'Unarchive' : 'Archive'}
        </button>
      ) : null,
    ];
    return menuGroups([session_, work, settings, end]);
  };

  /** The session menu: the confirms replace the items; the phone adds the facts. */
  const menuBody = (close: () => void) => {
    if (confirm === 'kill') {
      return (
        <KillConfirm
          busy={busy !== null}
          onKill={() => {
            setConfirm(null);
            close();
            run('stop', (a) => a.stop('kill'));
          }}
          onCancel={() => setConfirm(null)}
        />
      );
    }
    if (confirm === 'open' && terminal) return openConfirm(close, terminal.onOpen);
    return (
      <>
        {phone ? (
          <>
            <div className="fd-menu-text">
              <div className="fd-sh-badges">
                <Badges session={session} modeLast />
                {flags.held ? (
                  <span className="fd-badge" data-tone="act">
                    In terminal
                  </span>
                ) : null}
              </div>
              {place}
            </div>
            <div className="fd-menu-sep" />
          </>
        ) : null}
        {/* The Properties panel shows the facts while it is open. */}
        {panelOpen ? null : (
          <>
            <Details session={session} fleet={fleetControl?.(close)} />
            <div className="fd-menu-sep" />
          </>
        )}
        {menuItems(close)}
      </>
    );
  };

  /** The "Terminal" menu of the desktop header: the shell item, and the TUI handover when it is possible. */
  const showTerminalMenu =
    !guest && (onNewShell !== undefined || (canTerminalOpen && terminal !== null && !flags.held));
  const terminalMenu = showTerminalMenu ? (
    <Menu
      summary={
        <>
          <Icon of={SquareTerminal} /> <span className="fd-sh-wide">Terminal</span>
        </>
      }
      summaryClass="fd-ghost"
      label="Terminal"
      onClosed={() => setTermConfirm(false)}
    >
      {(close) =>
        termConfirm && terminal ? (
          openConfirm(close, terminal.onOpen, () => setTermConfirm(false))
        ) : (
          <>
            {onNewShell ? (
              <button
                type="button"
                onClick={() => {
                  close();
                  onNewShell();
                }}
              >
                <Icon of={SquareTerminal} /> Open shell in workspace
              </button>
            ) : null}
            {canTerminalOpen && terminal && !flags.held ? (
              <>
                {onNewShell ? <div className="fd-menu-sep" /> : null}
                <button type="button" disabled={busy !== null} onClick={() => setTermConfirm(true)}>
                  <Icon of={SquareTerminal} /> Open in terminal…
                </button>
              </>
            ) : null}
          </>
        )
      }
    </Menu>
  ) : null;

  const overflow = (
    <Menu
      summary={<Icon of={Ellipsis} />}
      summaryClass="fd-iconbtn"
      label="Session menu"
      onClosed={() => setConfirm(null)}
    >
      {menuBody}
    </Menu>
  );

  return (
    <div className="fd-sh-block">
      <header className="fd-sh" aria-label="Session header" data-layout={layout}>
        {onBack ? (
          <button type="button" className="fd-iconbtn" aria-label="Back" onClick={onBack}>
            <Icon of={ChevronLeft} />
          </button>
        ) : null}
        <span className="fd-holder" data-status={session.status} aria-hidden="true" />
        {phone ? null : (
          <div className="fd-sh-call">
            {s.callsign ? <span className="fd-callsign fd-sh-callsign">{s.callsign}</span> : null}
            <Who session={session} />
          </div>
        )}
        <div className="fd-sh-id">
          {phone && s.callsign ? (
            <span className="fd-callsign fd-sh-callsign">{s.callsign}</span>
          ) : null}
          <div className="fd-sh-line">
            {renaming ? (
              <RenameForm
                title={s.title}
                enabled={canRename}
                startEditing
                onClose={() => setRenaming(false)}
                onRename={(t) => run('rename', (a) => a.rename(t))}
              />
            ) : (
              <>
                <h2 className="fd-sh-title">{title}</h2>
                {s.title !== null && s.title_generated ? <GeneratedMark /> : null}
                {starred ? (
                  <span className="fd-sh-star" aria-hidden="true">
                    <Icon of={Star} size={14} />
                  </span>
                ) : null}
                {phone || guest ? null : (
                  <button
                    type="button"
                    className="fd-iconbtn fd-sm"
                    aria-label="Rename"
                    disabled={!canRename}
                    onClick={() => setRenaming(true)}
                  >
                    <Icon of={Pencil} />
                  </button>
                )}
              </>
            )}
          </div>
          {phone ? null : (
            <div className="fd-sh-chips">
              <Facts session={session} />
              {place}
            </div>
          )}
        </div>
        {phone && !renaming ? <StatusFact session={session} /> : null}
        <div className="fd-sh-actions">
          <Avatars
            who={
              people.presence[
                sessionKey({ station_id: session.station_id, session_id: s.session_id })
              ] ?? []
            }
            self={people.self}
          />
          {flags.canInterrupt && mayPrompt ? (
            phone ? (
              <button
                type="button"
                className="fd-iconbtn"
                aria-label="Interrupt"
                title="Interrupt"
                disabled={busy !== null}
                onClick={() => run('interrupt', (a) => a.interrupt())}
              >
                <Icon of={CircleStop} />
              </button>
            ) : (
              <button
                type="button"
                className="fd-ghost"
                disabled={busy !== null}
                onClick={() => run('interrupt', (a) => a.interrupt())}
              >
                <Icon of={CircleStop} size={14} /> Interrupt
              </button>
            )
          ) : null}
          {flags.canResume && !guest && !composerResume ? (
            phone ? (
              <button
                type="button"
                className="fd-iconbtn fd-primary"
                aria-label="Resume"
                disabled={busy !== null}
                onClick={() => run('resume', (a) => a.resume())}
              >
                <Icon of={Play} />
              </button>
            ) : (
              <button
                type="button"
                className="fd-primary"
                disabled={busy !== null}
                onClick={() => run('resume', (a) => a.resume())}
              >
                <Icon of={Play} size={14} /> Resume
              </button>
            )
          ) : null}
          {phone ? null : flags.held ? (
            <>
              <span className="fd-badge" data-tone="act">
                In terminal
              </span>
              {terminal ? (
                <button
                  type="button"
                  disabled={!flags.live || actions?.canDrive !== true || busy !== null}
                  onClick={terminal.onReturn}
                >
                  <Icon of={Undo2} size={14} /> Return to board control
                </button>
              ) : null}
              {terminalMenu}
            </>
          ) : (
            terminalMenu
          )}
          {filesPanel ? (
            <button
              type="button"
              className="fd-iconbtn"
              aria-label="Files"
              title="Files"
              aria-pressed={filesPanel.open}
              onClick={filesPanel.toggle}
            >
              <Icon of={FolderTree} />
            </button>
          ) : null}
          {propsPanel ? (
            <button
              type="button"
              className="fd-iconbtn"
              aria-label="Properties"
              title="Properties"
              aria-pressed={propsPanel.open}
              onClick={propsPanel.toggle}
            >
              <Icon of={PanelRight} />
            </button>
          ) : null}
          {overflow}
        </div>
      </header>
      {flags.live ? null : (
        <p className="fd-sh-strip" data-tone="act">
          Controls are off until the board and the station are live.
        </p>
      )}
      {busy === 'handover' ? (
        <p className="fd-sh-strip" data-tone="info" role="status">
          Stopping the session, then starting its terminal…
        </p>
      ) : null}
      {busy === 'return' ? (
        <p className="fd-sh-strip" data-tone="info" role="status">
          Ending the terminal, then resuming the session…
        </p>
      ) : null}
      {showError && s.last_error ? (
        <p className="fd-sh-strip" data-tone="hazard" role="status">
          <Icon of={TriangleAlert} size={14} /> {s.last_error.code}: {s.last_error.message}
          {retry ? ` · ${retry}` : ''}
        </p>
      ) : null}
      {outcome ? (
        <div role={outcome.ok ? 'status' : 'alert'} className="fd-sh-outcome" data-ok={outcome.ok}>
          <Icon of={outcome.ok ? Check : TriangleAlert} size={14} />
          <span>{outcome.text}</span>
          <button
            type="button"
            className="fd-iconbtn fd-sm"
            aria-label="Dismiss"
            onClick={controller.dismiss}
          >
            <Icon of={X} />
          </button>
        </div>
      ) : null}
      {moving ? <MoveToProject session={session} onClose={() => setMoving(false)} /> : null}
      {movingStation ? (
        <MoveDialog
          session={session}
          onClose={() => setMovingStation(false)}
          onStarted={() => onMoveStarted?.()}
        />
      ) : null}
      {accessOpen && fleet !== undefined ? (
        <SessionAccessDialog
          session={session}
          stations={stations}
          actions={fleet}
          onClose={() => setAccessOpen(false)}
        />
      ) : null}
      {prOpen && prProject !== undefined ? (
        <OpenPrDialog project={prProject} session={session} onClose={() => setPrOpen(false)} />
      ) : null}
    </div>
  );
}
