import { useCallback, useEffect, useState } from 'react';
import type { ShellInfo } from '../gen';
import { useTouch } from '../app/viewport';
import { Terminal } from '../terminal/Terminal';
import type { ShellActions } from './shell';

type Props = {
  readonly station: string;
  readonly actions: ShellActions;
  /** The link is down or the board is stale. */
  readonly disabled: boolean;
};

const message = (e: unknown) => (e instanceof Error ? e.message : 'Shell request failed');

/**
 * The owner's shells on one station (PLAN §6.8, D10). The station owns the PTY; the
 * shell keeps running when this view closes, and a new view receives the scrollback.
 */
export function ShellPanel({ station, actions, disabled }: Props) {
  const [shells, setShells] = useState<ShellInfo[] | null>(null);
  const [cwd, setCwd] = useState('');
  const [active, setActive] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const touch = useTouch();
  const refresh = useCallback(async () => {
    try {
      setShells(await actions.list(station));
    } catch (e) {
      setError(message(e));
    }
  }, [actions, station]);
  useEffect(() => {
    if (!disabled) void refresh();
  }, [disabled, refresh]);
  const run = (work: () => Promise<void>) => {
    setError(null);
    void work()
      .catch((e: unknown) => setError(message(e)))
      .then(refresh);
  };
  return (
    <section className="fd-shells" aria-label={`Shells on ${station}`}>
      <h4>Shells</h4>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          const dir = cwd.trim();
          if (dir === '') return;
          run(async () => setActive(await actions.open(station, dir, 80, 24)));
        }}
      >
        <label>
          Directory{' '}
          <input
            value={cwd}
            placeholder="/path/inside/an/allowed/root"
            onChange={(e) => setCwd(e.target.value)}
          />
        </label>{' '}
        <button type="submit" disabled={disabled || cwd.trim() === ''}>
          Open shell
        </button>
      </form>
      {error === null ? null : <p role="alert">{error}</p>}
      {shells === null || shells.length === 0 ? (
        <p>No open shells.</p>
      ) : (
        <ul className="fd-shell-list">
          {shells.map((s) => (
            <li key={s.shell_id} data-shell={s.shell_id}>
              {s.cwd} · {s.state === 'running' ? 'running' : `exited (${s.exit_code ?? '?'})`}{' '}
              <button
                type="button"
                disabled={disabled || active === s.shell_id}
                onClick={() => setActive(s.shell_id)}
              >
                Attach
              </button>{' '}
              <button
                type="button"
                disabled={disabled}
                onClick={() =>
                  run(async () => {
                    if (active === s.shell_id) setActive(null);
                    await actions.close(station, s.shell_id);
                  })
                }
              >
                Close shell
              </button>
            </li>
          ))}
        </ul>
      )}
      {active === null ? null : (
        <>
          <button type="button" onClick={() => setActive(null)}>
            Detach
          </button>
          <div className="fd-terminal">
            <Terminal
              station={station}
              shellId={active}
              actions={actions}
              active={true}
              touch={touch}
            />
          </div>
        </>
      )}
    </section>
  );
}
