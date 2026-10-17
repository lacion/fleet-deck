// DL-140: the header of a session: title, status, provider, model, where it
// runs, and the actions (interrupt, resume, terminal) with one overflow menu (DL-141).
import { useEffect, useState } from 'react';
import { formatHash } from '../app/routes';
import { relativeAge } from '../app/sessions';
import { stationName } from '../board/fleet';
import type { DeckSession, ModelChoice, StationView } from '../gen';
import { digestLines, GeneratedMark } from '../ui/generated';
import { Menu } from '../ui/Menu';
import {
  Archive,
  ArchiveRestore,
  Check,
  ChevronLeft,
  Copy,
  Ellipsis,
  FileText,
  Folder,
  FolderGit2,
  GitCompare,
  Icon,
  Pencil,
  Play,
  Power,
  Search,
  Server,
  Send,
  Square,
  SquareTerminal,
  Star,
  TriangleAlert,
  Undo2,
  Users,
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
import { Badges, Place } from './HeaderBadges';
import { MoveDialog } from './MoveDialog';
import { MoveToProject } from './MoveToProject';
import { RenameForm } from './RenameForm';
import { ModeLadder, ModelPicker } from './SessionSettings';

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
  /** The models that the provider harness reports (`session.models`). */
  readonly models?: readonly ModelChoice[] | null;
  /** Opens the check of the previous run (DL-182); the menu offers it when given (owner only). */
  readonly onCheckExecution?: (() => void) | undefined;
  /** FG-9: a move started from the menu; the session view reads the move again. */
  readonly onMoveStarted?: (() => void) | undefined;
};

/** The status update that "Ask for a status update" sends. */
const NUDGE =
  'Give a short status update: what you finished, what you do now, and what blocks you.';

const COPIED_MS = 1500;

/** The longest path the header shows, in characters. */
const MAX_PATH_CHARS = 48;

/** A long path loses its middle: `/home/lu…/repo/src`. */
function shortenMiddle(text: string, max: number): string {
  const chars = [...text];
  if (chars.length <= max) return text;
  const keep = max - 1;
  const head = Math.ceil(keep / 2);
  const tail = keep - head;
  return `${chars.slice(0, head).join('')}…${chars.slice(chars.length - tail).join('')}`;
}

/** An icon button that copies `text`; it shows a check for 1.5 s. */
export function CopyButton({ text, label }: { readonly text: string; readonly label: string }) {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), COPIED_MS);
    return () => clearTimeout(timer);
  }, [copied]);
  return (
    <button
      type="button"
      className="fd-iconbtn fd-sm"
      aria-label={label}
      title={label}
      onClick={() => {
        navigator.clipboard
          .writeText(text)
          .then(() => setCopied(true))
          .catch(() => setCopied(false));
      }}
    >
      <Icon of={copied ? Check : Copy} size={14} />
    </button>
  );
}

/** "retry in 3 min" (rounded up), or "retry in 40 s" under a minute; `null` when the time is past. */
function retryText(retryAt: number | null, now: number): string | null {
  if (retryAt === null || retryAt <= now) return null;
  const ms = retryAt - now;
  return ms < 60_000
    ? `retry in ${Math.ceil(ms / 1000)} s`
    : `retry in ${Math.ceil(ms / 60_000)} min`;
}

/** The facts of a session: its id (with a copy button), callsign, worktree, age and event count. */
function Details({ session, now }: { readonly session: DeckSession; readonly now: number }) {
  const s = session.summary;
  const digest = digestLines(s.digest);
  return (
    <dl className="fd-sh-details">
      <div>
        <dt>Session</dt>
        <dd>
          <span>{s.session_id}</span>
          <CopyButton text={s.session_id} label="Copy the session id" />
        </dd>
      </div>
      {s.callsign ? (
        <div>
          <dt>Callsign</dt>
          <dd>{s.callsign}</dd>
        </div>
      ) : null}
      {s.worktree ? (
        <div>
          <dt>Worktree</dt>
          <dd>{s.worktree}</dd>
        </div>
      ) : null}
      {digest.length > 0 ? (
        <div data-field="digest">
          <dt>Summary (generated)</dt>
          <dd>
            <ul className="fd-sh-digest">
              {digest.map((line) => (
                <li key={line}>{line}</li>
              ))}
            </ul>
          </dd>
        </div>
      ) : null}
      <div>
        <dt>Updated</dt>
        <dd>{relativeAge(s.updated_at, now)}</dd>
      </div>
      <div>
        <dt>Events</dt>
        <dd>{s.session_seq}</dd>
      </div>
    </dl>
  );
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
  models = null,
  onCheckExecution,
  onMoveStarted,
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
  const place = [
    stationName(session.station_id, stations),
    s.branch,
    s.cwd ? shortenMiddle(s.cwd, MAX_PATH_CHARS) : null,
  ].filter((part): part is string => part !== null && part !== '');
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
    <div className="fd-sh-confirm" role="group" aria-label="Open in terminal">
      <p className="fd-menu-text">
        In the terminal, the board does not see approvals or prompts. The station holds this session
        until you return it.
        {flags.ended ? '' : ' The session stops first.'}
      </p>
      <button
        type="button"
        className="fd-primary"
        disabled={busy !== null}
        onClick={() => {
          reset();
          close();
          open();
        }}
      >
        Open in terminal
      </button>
      <button
        type="button"
        onClick={() => {
          reset();
          close();
        }}
      >
        Cancel
      </button>
    </div>
  );

  /** The items of the session menu that every layout has. */
  const menuItems = (close: () => void) => (
    <>
      {guest ? null : (
        <button
          type="button"
          disabled={!canRename}
          onClick={() => {
            close();
            setRenaming(true);
          }}
        >
          <Icon of={Pencil} /> Rename
        </button>
      )}
      {flags.canStop && !guest ? (
        <>
          <button
            type="button"
            disabled={busy !== null}
            onClick={() => {
              close();
              run('stop', (a) => a.stop('graceful'));
            }}
          >
            <Icon of={Power} /> Stop
          </button>
          <button
            type="button"
            className="fd-danger"
            disabled={busy !== null}
            onClick={() => setConfirm('kill')}
          >
            Force stop…
          </button>
        </>
      ) : null}
      {!guest && onCheckExecution !== undefined ? (
        <button
          type="button"
          onClick={() => {
            close();
            onCheckExecution();
          }}
        >
          <Icon of={Search} /> Check the previous run
        </button>
      ) : null}
      {flags.drive && mayPrompt ? (
        <button
          type="button"
          disabled={busy !== null}
          onClick={() => {
            close();
            run('nudge', (a) => a.send(s.current_run === null ? 'prompt' : 'follow_up', NUDGE));
          }}
        >
          <Icon of={Send} /> Ask for a status update
        </button>
      ) : null}
      {s.batch !== undefined && s.batch.size >= 2 && !guest ? (
        <button
          type="button"
          onClick={() => {
            close();
            window.location.hash = formatHash({ name: 'compare', batch: s.batch?.batch_id ?? '' });
          }}
        >
          <Icon of={GitCompare} /> Compare batch
        </button>
      ) : null}
      {flags.live && actions?.canDrive === true ? (
        <button
          type="button"
          disabled={busy !== null}
          onClick={() => {
            close();
            run('summarize', (a) => a.summarize());
          }}
        >
          <Icon of={FileText} /> Summarize
        </button>
      ) : null}
      {prProject !== undefined && !guest && actions?.canDrive === true && s.branch ? (
        <button
          type="button"
          onClick={() => {
            close();
            setPrOpen(true);
          }}
        >
          <Icon of={GitPullRequest} /> Open a pull request…
        </button>
      ) : null}
      <button type="button" onClick={() => priority.toggle(ref)}>
        <Icon of={Star} /> {starred ? 'Unstar' : 'Star'}
      </button>
      {canShare(people) ? (
        <button
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
          <Icon of={Users} /> Share session…
        </button>
      ) : null}
      {canMove ? (
        <button
          type="button"
          onClick={() => {
            close();
            setMoving(true);
          }}
        >
          <Icon of={FolderGit2} /> Move to project…
        </button>
      ) : null}
      {canMoveStation ? (
        <button
          type="button"
          onClick={() => {
            close();
            setMovingStation(true);
          }}
        >
          <Icon of={Server} /> Move to station…
        </button>
      ) : null}
      {canArchive ? (
        <button type="button" onClick={() => archived.toggle(ref)}>
          <Icon of={isArchived ? ArchiveRestore : Archive} /> {isArchived ? 'Unarchive' : 'Archive'}
        </button>
      ) : null}
      {hasModes || hasModel || hasAuto ? (
        <>
          <div className="fd-menu-sep" />
          {hasModes && caps ? (
            <ModeLadder
              modes={caps.modes}
              current={s.mode}
              enabled={settingsEnabled}
              onSet={(m) => run('mode', (a) => a.setMode(m))}
            />
          ) : null}
          {hasModel ? (
            <ModelPicker
              current={s.model}
              models={models}
              enabled={settingsEnabled}
              onSet={(m) => run('model', (a) => a.setModel(m))}
            />
          ) : null}
          {hasAuto ? (
            <label className="fd-auto-continue">
              <input
                type="checkbox"
                checked={s.auto_continue}
                disabled={!flags.live || actions?.canDrive !== true || busy !== null}
                onChange={(event) => {
                  const on = event.currentTarget.checked;
                  run('auto-continue', (a) => a.setAutoContinue(on));
                }}
              />{' '}
              Continue after a station restart
            </label>
          ) : null}
        </>
      ) : null}
      {onNewShell && !guest ? (
        <>
          <div className="fd-menu-sep" />
          <button
            type="button"
            onClick={() => {
              close();
              onNewShell();
            }}
          >
            <Icon of={SquareTerminal} /> Open shell in workspace
          </button>
        </>
      ) : null}
      {terminal !== null && !terminal.available && !guest ? (
        <>
          <div className="fd-menu-sep" />
          <p className="fd-menu-text">
            Shells are off on this station. Set allow_shell = true in station.toml to open a
            terminal.
          </p>
        </>
      ) : null}
    </>
  );

  /** The session menu: the confirms replace the items; the phone adds the facts and the terminal item. */
  const menuBody = (close: () => void) => {
    if (confirm === 'kill') {
      return (
        <div className="fd-sh-confirm" role="group" aria-label="Force stop">
          <p className="fd-menu-text">Force stop kills the provider process. Continue?</p>
          <button
            type="button"
            className="fd-danger"
            disabled={busy !== null}
            onClick={() => {
              setConfirm(null);
              close();
              run('stop', (a) => a.stop('kill'));
            }}
          >
            Force stop
          </button>
          <button type="button" onClick={() => setConfirm(null)}>
            Cancel
          </button>
        </div>
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
              <Place parts={place} cwd={s.cwd} />
            </div>
            <div className="fd-menu-sep" />
            <Details session={session} now={now} />
            <div className="fd-menu-sep" />
            {guest ? null : flags.held && terminal ? (
              <button
                type="button"
                disabled={!flags.live || actions?.canDrive !== true || busy !== null}
                onClick={() => {
                  close();
                  terminal.onReturn();
                }}
              >
                <Icon of={Undo2} /> Return to board control
              </button>
            ) : !flags.held && canTerminalOpen && terminal ? (
              <button type="button" disabled={busy !== null} onClick={() => setConfirm('open')}>
                <Icon of={SquareTerminal} /> Open in terminal…
              </button>
            ) : null}
          </>
        ) : (
          <>
            <Details session={session} now={now} />
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
        <div className="fd-sh-id">
          <div className="fd-sh-line">
            <span className="fd-dot" data-status={session.status} aria-hidden="true" />
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
            {phone ? null : <Badges session={session} />}
          </div>
          {phone ? null : <Place parts={place} cwd={s.cwd} />}
        </div>
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
                disabled={busy !== null}
                onClick={() => run('interrupt', (a) => a.interrupt())}
              >
                <Icon of={Square} />
              </button>
            ) : (
              <button
                type="button"
                disabled={busy !== null}
                onClick={() => run('interrupt', (a) => a.interrupt())}
              >
                <Icon of={Square} size={14} /> Interrupt
              </button>
            )
          ) : null}
          {flags.canResume && !guest ? (
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
              <Icon of={Folder} />
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
      {prOpen && prProject !== undefined ? (
        <OpenPrDialog project={prProject} session={session} onClose={() => setPrOpen(false)} />
      ) : null}
    </div>
  );
}
