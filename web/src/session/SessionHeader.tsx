// DL-140: the header of a session: title, status, provider, model, where it
// runs, and the actions (interrupt, stop, resume, terminal, settings).
import { type KeyboardEvent, type ReactNode, useRef, useState } from 'react';
import { stationName } from '../board/fleet';
import { providerLabel } from '../board/lanes';
import type { DeckSession, StationView } from '../gen';
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
      <summary aria-label={props.label}>{props.summary}</summary>
      <div className="fd-menu-body">{props.children(close)}</div>
    </details>
  );
}

function PencilIcon() {
  return (
    <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true" focusable="false">
      <path
        d="M11.5 2.5l2 2L5 13H3v-2z"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export function SessionHeader({ session, stations, controller, actions, onBack, terminal }: Props) {
  const s = session.summary;
  const caps = s.capabilities;
  const { flags, busy } = controller;
  const [renaming, setRenaming] = useState(false);
  const [confirmKill, setConfirmKill] = useState(false);
  const [confirmOpen, setConfirmOpen] = useState(false);
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

  return (
    <div className="fd-sh-block">
      <header className="fd-sh" aria-label="Session header">
        <div className="fd-sh-line">
          {onBack ? (
            <button type="button" className="fd-sh-back" aria-label="Back" onClick={onBack}>
              <span aria-hidden="true">←</span>
            </button>
          ) : null}
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
              <button
                type="button"
                className="fd-sh-icon"
                aria-label="Rename"
                disabled={!canRename}
                onClick={() => setRenaming(true)}
              >
                <PencilIcon />
              </button>
            </>
          )}
          <span className="fd-sh-status" data-status={session.status}>
            {session.status.replaceAll('_', ' ')}
          </span>
          <span className="fd-sh-chip" data-provider={s.provider}>
            {providerLabel(s.provider)}
          </span>
          <span className="fd-sh-chip fd-sh-model">{s.model ?? 'default model'}</span>
        </div>
        <p className="fd-sh-place">
          {place.map((part, n) => (
            <span key={part} title={n === place.length - 1 && s.cwd ? s.cwd : undefined}>
              {n > 0 ? ' · ' : ''}
              {part}
            </span>
          ))}
        </p>
        <div className="fd-sh-actions">
          {flags.canInterrupt ? (
            <button
              type="button"
              disabled={busy !== null}
              onClick={() => run('interrupt', (a) => a.interrupt())}
            >
              Interrupt
            </button>
          ) : null}
          {flags.canStop ? (
            <Menu summary="Stop" onClosed={() => setConfirmKill(false)}>
              {(close) =>
                confirmKill ? (
                  <div className="fd-sh-confirm" role="group" aria-label="Force stop">
                    <p>Force stop kills the provider process. Continue?</p>
                    <div className="fd-options">
                      <button
                        type="button"
                        className="fd-danger"
                        disabled={busy !== null}
                        onClick={() => {
                          setConfirmKill(false);
                          close();
                          run('stop', (a) => a.stop('kill'));
                        }}
                      >
                        Force stop
                      </button>
                      <button type="button" onClick={() => setConfirmKill(false)}>
                        Cancel
                      </button>
                    </div>
                  </div>
                ) : (
                  <>
                    <button
                      type="button"
                      disabled={busy !== null}
                      onClick={() => {
                        close();
                        run('stop', (a) => a.stop('graceful'));
                      }}
                    >
                      Stop
                    </button>
                    <button
                      type="button"
                      className="fd-danger"
                      disabled={busy !== null}
                      onClick={() => setConfirmKill(true)}
                    >
                      Force stop
                    </button>
                  </>
                )
              }
            </Menu>
          ) : null}
          {flags.canResume ? (
            <button
              type="button"
              disabled={busy !== null}
              onClick={() => run('resume', (a) => a.resume())}
            >
              Resume
            </button>
          ) : null}
          {flags.held ? (
            <>
              <span className="fd-sh-badge">In terminal</span>
              {terminal ? (
                <button
                  type="button"
                  disabled={!flags.live || actions?.canDrive !== true || busy !== null}
                  onClick={terminal.onReturn}
                >
                  Return to board control
                </button>
              ) : null}
            </>
          ) : canTerminalOpen && terminal ? (
            confirmOpen ? (
              <div className="fd-sh-confirm" role="group" aria-label="Open in terminal">
                <p>
                  In the terminal, the board does not see approvals or prompts. The station holds
                  this session until you return it.
                  {flags.ended ? '' : ' The session stops first.'}
                </p>
                <div className="fd-options">
                  <button
                    type="button"
                    disabled={busy !== null}
                    onClick={() => {
                      setConfirmOpen(false);
                      terminal.onOpen();
                    }}
                  >
                    Open in terminal
                  </button>
                  <button type="button" onClick={() => setConfirmOpen(false)}>
                    Cancel
                  </button>
                </div>
              </div>
            ) : (
              <button type="button" disabled={busy !== null} onClick={() => setConfirmOpen(true)}>
                Open in terminal
              </button>
            )
          ) : null}
          {hasModes || hasModel || hasAuto ? (
            <Menu summary={<span aria-hidden="true">⚙</span>} label="Session settings">
              {() => (
                <>
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
              )}
            </Menu>
          ) : null}
        </div>
        {flags.live ? null : (
          <p className="fd-sh-off">Controls are off until the board and the station are live.</p>
        )}
      </header>
      {outcome ? (
        <div role={outcome.ok ? 'status' : 'alert'} className="fd-sh-outcome" data-ok={outcome.ok}>
          <span>{outcome.text}</span>
          <button type="button" onClick={controller.dismiss}>
            Dismiss
          </button>
        </div>
      ) : null}
    </div>
  );
}
