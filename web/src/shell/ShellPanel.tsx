import { useCallback, useEffect, useRef, useState } from 'react';
import type { ShellInfo } from '../gen';
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
          <ShellTerminal station={station} shellId={active} actions={actions} />
        </>
      )}
    </section>
  );
}

/** An xterm.js view of one shell stream: binary frames both ways. */
function ShellTerminal({
  station,
  shellId,
  actions,
}: {
  readonly station: string;
  readonly shellId: string;
  readonly actions: ShellActions;
}) {
  const host = useRef<HTMLDivElement>(null);
  useEffect(() => {
    let disposed = false;
    let cleanup = () => {};
    void (async () => {
      const [{ Terminal }, { FitAddon }] = await Promise.all([
        import('@xterm/xterm'),
        import('@xterm/addon-fit'),
      ]);
      const el = host.current;
      if (disposed || el === null) return;
      const term = new Terminal({ cursorBlink: true, fontSize: 13, scrollback: 5000 });
      const fit = new FitAddon();
      term.loadAddon(fit);
      term.open(el);
      fit.fit();
      const url = new URL(actions.socketPath(station, shellId), window.location.href);
      url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
      const ws = new WebSocket(url);
      ws.binaryType = 'arraybuffer';
      ws.onmessage = (e: MessageEvent) => {
        const data: unknown = e.data;
        // A tag check, not `instanceof`: the buffer can come from another realm.
        if (Object.prototype.toString.call(data) === '[object ArrayBuffer]')
          term.write(new Uint8Array(data as ArrayBuffer));
        else if (data instanceof Blob)
          void data.arrayBuffer().then((b) => term.write(new Uint8Array(b)));
        else if (typeof data === 'string') term.write(data);
      };
      ws.onclose = () => term.write('\r\n[shell stream closed]\r\n');
      const encoder = new TextEncoder();
      const input = term.onData((d) => {
        if (ws.readyState === WebSocket.OPEN) ws.send(encoder.encode(d));
      });
      const resized = term.onResize(({ cols, rows }) => {
        void actions.resize(station, shellId, cols, rows).catch(() => undefined);
      });
      const onWindow = () => fit.fit();
      window.addEventListener('resize', onWindow);
      void actions.resize(station, shellId, term.cols, term.rows).catch(() => undefined);
      cleanup = () => {
        window.removeEventListener('resize', onWindow);
        input.dispose();
        resized.dispose();
        ws.close();
        term.dispose();
      };
    })();
    return () => {
      disposed = true;
      cleanup();
    };
  }, [station, shellId, actions]);
  return <div className="fd-terminal" ref={host} data-terminal={shellId} />;
}
