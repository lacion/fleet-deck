// DL-140: the header of a session: title, status, provider, model, where it
// runs, and the actions (interrupt, resume, terminal) with one overflow menu (DL-141).
import { type KeyboardEvent, type ReactNode, useRef, useState } from 'react';
import { stationName } from '../board/fleet';
import { providerLabel } from '../board/lanes';
import type { DeckSession, StationView } from '../gen';
import { modelFamily, shortModel } from '../ui/format';
import {
  Check,
  ChevronLeft,
  Ellipsis,
  Folder,
  Icon,
  Pencil,
  Play,
  Power,
  Square,
  SquareTerminal,
  TriangleAlert,
  Undo2,
  X,
} from '../ui/icons';
import type { Outcome, SessionActions } from './commands';
import type { Controller } from './controller';
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
};

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

/** A `<details>` menu. Escape closes it and returns the focus to its summary. */
function Menu(props: {
  readonly summary: ReactNode;
  readonly label?: string;
  readonly summaryClass?: string;
  readonly onClosed?: () => void;
  readonly children: (close: () => void) => ReactNode;
}) {
  const ref = useRef<HTMLDetailsElement>(null);
  const close = () => {
    if (ref.current) ref.current.open = false;
  };
  const onKeyDown = (event: KeyboardEvent<HTMLDetailsElement>) => {
    if (event.key !== 'Escape' || !ref.current?.open) return;
    event.stopPropagation();
    close();
    ref.current.querySelector('summary')?.focus();
  };
  return (
    <details
      className="fd-menu"
      ref={ref}
      onKeyDown={onKeyDown}
      onToggle={(event) => {
        if (!event.currentTarget.open) props.onClosed?.();
      }}
    >
      <summary className={props.summaryClass} aria-label={props.label}>
        {props.summary}
      </summary>
      <div className="fd-menu-body">{props.children(close)}</div>
    </details>
  );
}

/** The tone of the status badge. */
function statusTone(status: string): 'act' | 'ok' | 'info' | 'hazard' | 'muted' {
  switch (status) {
    case 'needs_you':
      return 'act';
    case 'working':
      return 'ok';
    case 'verifying':
      return 'info';
    case 'unreachable':
    case 'lost':
      return 'hazard';
    default:
      return 'muted';
  }
}

/** The tone of the permission mode badge. */
function modeTone(mode: string): 'plan' | 'info' | 'hazard' | 'muted' {
  switch (mode) {
    case 'plan':
      return 'plan';
    case 'acceptEdits':
      return 'info';
    case 'bypassPermissions':
    case 'dontAsk':
      return 'hazard';
    default:
      return 'muted';
  }
}

/** The status, model, mode and provider badges of a session (the phone menu puts the mode last). */
function Badges({
  session,
  modeLast = false,
}: {
  readonly session: DeckSession;
  readonly modeLast?: boolean;
}) {
  const s = session.summary;
  const mode = s.mode ? (
    <span className="fd-badge" data-tone={modeTone(s.mode)} title="Permission mode">
      {s.mode}
    </span>
  ) : null;
  return (
    <>
      <span className="fd-badge" data-tone={statusTone(session.status)}>
        {session.status.replaceAll('_', ' ')}
      </span>
      <span
        className="fd-badge"
        data-family={modelFamily(s.model) ?? undefined}
        title={s.model ?? 'default model'}
      >
        {shortModel(s.model)}
      </span>
      {modeLast ? null : mode}
      <span className="fd-badge" data-provider={s.provider}>
        {providerLabel(s.provider)}
      </span>
      {modeLast ? mode : null}
    </>
  );
}

/** The station, the branch and the directory of a session. */
function Place({ parts, cwd }: { readonly parts: readonly string[]; readonly cwd: string | null }) {
  return (
    <p className="fd-sh-place">
      {parts.map((part, n) => (
        <span key={part} title={n === parts.length - 1 && cwd ? cwd : undefined}>
          {n > 0 ? ' · ' : ''}
          {part}
        </span>
      ))}
    </p>
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
}: Props) {
  const s = session.summary;
  const caps = s.capabilities;
  const { flags, busy } = controller;
  const phone = layout === 'phone';
  const [renaming, setRenaming] = useState(false);
  const [confirm, setConfirm] = useState<'kill' | 'open' | null>(null);
  const run = (name: string, call: (a: SessionActions) => Promise<Outcome>) => {
    if (actions) controller.act(name, () => call(actions));
  };
  const title = s.title ?? s.callsign ?? s.session_id;
  const canRename = flags.live && actions?.canDrive === true && busy === null;
  const canTerminalOpen =
    terminal?.available === true &&
    caps?.resume === true &&
    flags.live &&
    actions?.canDrive === true;
  const settingsEnabled = flags.drive && busy === null;
  const hasModes = caps?.set_mode === true && caps.modes.length > 0;
  const hasModel = caps?.set_model === true;
  const hasAuto = caps?.resume === true;
  const place = [
    stationName(session.station_id, stations),
    s.branch,
    s.cwd ? shortenMiddle(s.cwd, MAX_PATH_CHARS) : null,
  ].filter((part): part is string => part !== null && part !== '');
  const outcome = controller.outcome;

  /** The confirm of "Open in terminal", inside a menu. */
  const openConfirm = (close: () => void, open: () => void) => (
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
          setConfirm(null);
          close();
          open();
        }}
      >
        Open in terminal
      </button>
      <button
        type="button"
        onClick={() => {
          setConfirm(null);
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
      {flags.canStop ? (
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
      {onNewShell ? (
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
            {flags.held && terminal ? (
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
        ) : null}
        {menuItems(close)}
      </>
    );
  };

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
                {phone ? null : (
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
          {flags.canInterrupt ? (
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
          {flags.canResume ? (
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
            </>
          ) : canTerminalOpen && terminal ? (
            <Menu
              summary={
                <>
                  <Icon of={SquareTerminal} /> <span className="fd-sh-wide">Open in terminal</span>
                </>
              }
              label="Open in terminal"
            >
              {(close) => openConfirm(close, terminal.onOpen)}
            </Menu>
          ) : null}
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
    </div>
  );
}
