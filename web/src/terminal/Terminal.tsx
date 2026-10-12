import { useEffect, useRef, useState } from 'react';
import type { ShellActions } from '../shell/shell';
import { KeyBar, ctrlByte } from './KeyBar';

type Props = {
  readonly station: string;
  readonly shellId: string;
  readonly actions: ShellActions;
  /** Only the active terminal fits and takes the focus. */
  readonly active: boolean;
  /** A touch device: show the key bar. */
  readonly touch: boolean;
  /** Given: the closed overlay offers "Remove tab". */
  readonly onEnded?: () => void;
};

type Status = 'connecting' | 'live' | 'closed';

type Handles = { fit: () => void; focus: () => void };

const RESIZE_DEBOUNCE_MS = 150;

function theme(): Record<string, string> {
  const css = getComputedStyle(document.documentElement);
  const v = (name: string) => css.getPropertyValue(name).trim();
  const out: Record<string, string> = {};
  const set = (key: string, name: string) => {
    const value = v(name);
    if (value !== '') out[key] = value;
  };
  set('background', '--code');
  set('foreground', '--text');
  set('cursor', '--act');
  set('selectionBackground', '--border');
  return out;
}

/** An xterm.js view of one shell stream: binary frames both ways (PLAN §6.8). */
export function Terminal({ station, shellId, actions, active, touch, onEnded }: Props) {
  const host = useRef<HTMLDivElement>(null);
  const [status, setStatus] = useState<Status>('connecting');
  const [attempt, setAttempt] = useState(0);
  const [ctrl, setCtrlState] = useState(false);
  const ctrlRef = useRef(false);
  const activeRef = useRef(active);
  const handles = useRef<Handles | null>(null);
  const sender = useRef<(bytes: string) => void>(() => undefined);
  activeRef.current = active;
  const setCtrl = (on: boolean) => {
    ctrlRef.current = on;
    setCtrlState(on);
  };

  useEffect(() => {
    let disposed = false;
    let cleanup = () => {};
    setStatus('connecting');
    void (async () => {
      const [{ Terminal: XTerm }, { FitAddon }] = await Promise.all([
        import('@xterm/xterm'),
        import('@xterm/addon-fit'),
      ]);
      const el = host.current;
      if (disposed || el === null) return;
      const css = getComputedStyle(document.documentElement);
      const fontFamily = css.getPropertyValue('--font-data').trim();
      const term = new XTerm({
        cursorBlink: true,
        fontSize: 13,
        scrollback: 5000,
        ...(fontFamily === '' ? {} : { fontFamily }),
        theme: theme(),
      });
      const fitAddon = new FitAddon();
      term.loadAddon(fitAddon);
      term.open(el);
      const fit = () => {
        try {
          fitAddon.fit();
        } catch {
          // A hidden container has no size; the next fit (on activation) works.
        }
      };
      handles.current = { fit, focus: () => term.focus() };
      if (activeRef.current) {
        fit();
        term.focus();
      }
      const url = new URL(actions.socketPath(station, shellId), window.location.href);
      url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
      const ws = new WebSocket(url);
      ws.binaryType = 'arraybuffer';
      ws.onopen = () => {
        if (!disposed) setStatus('live');
      };
      ws.onmessage = (e: MessageEvent) => {
        const data: unknown = e.data;
        // A tag check, not `instanceof`: the buffer can come from another realm.
        if (Object.prototype.toString.call(data) === '[object ArrayBuffer]')
          term.write(new Uint8Array(data as ArrayBuffer));
        else if (data instanceof Blob)
          void data.arrayBuffer().then((b) => term.write(new Uint8Array(b)));
        else if (typeof data === 'string') term.write(data);
      };
      ws.onclose = () => {
        if (!disposed) setStatus('closed');
      };
      const encoder = new TextEncoder();
      const write = (bytes: string) => {
        if (ws.readyState === WebSocket.OPEN) ws.send(encoder.encode(bytes));
      };
      sender.current = write;
      const input = term.onData((d) => {
        if (ctrlRef.current && d.length === 1) {
          setCtrl(false);
          write(ctrlByte(d));
        } else write(d);
      });
      let sent: { cols: number; rows: number } | null = null;
      let timer: ReturnType<typeof setTimeout> | undefined;
      const sendSize = () => {
        timer = undefined;
        const { cols, rows } = term;
        if (sent !== null && sent.cols === cols && sent.rows === rows) return;
        sent = { cols, rows };
        void actions.resize(station, shellId, cols, rows).catch(() => undefined);
      };
      const schedule = () => {
        if (timer !== undefined) clearTimeout(timer);
        timer = setTimeout(sendSize, RESIZE_DEBOUNCE_MS);
      };
      const resized = term.onResize(schedule);
      schedule();
      const observer = new ResizeObserver(() => {
        if (activeRef.current) fit();
      });
      observer.observe(el);
      cleanup = () => {
        if (timer !== undefined) clearTimeout(timer);
        observer.disconnect();
        input.dispose();
        resized.dispose();
        ws.onclose = null;
        ws.close();
        term.dispose();
        handles.current = null;
        sender.current = () => undefined;
      };
    })();
    return () => {
      disposed = true;
      cleanup();
    };
  }, [station, shellId, actions, attempt]);

  // Becoming active: fit once and take the focus.
  useEffect(() => {
    if (!active) return;
    handles.current?.fit();
    handles.current?.focus();
  }, [active]);

  const send = (bytes: string) => {
    sender.current(bytes);
    handles.current?.focus();
  };
  return (
    <div className="fd-term" style={{ height: '100%' }} data-terminal={shellId}>
      <div className="fd-term-host" ref={host} />
      {status === 'connecting' ? (
        <div className="fd-term-overlay" role="status">
          <p>Connecting…</p>
        </div>
      ) : null}
      {status === 'closed' ? (
        <div className="fd-term-overlay" role="status">
          <p>The stream closed.</p>
          <div className="fd-term-actions">
            <button type="button" onClick={() => setAttempt((n) => n + 1)}>
              Reconnect
            </button>
            {onEnded === undefined ? null : (
              <button type="button" onClick={onEnded}>
                Remove tab
              </button>
            )}
          </div>
        </div>
      ) : null}
      {touch ? <KeyBar send={send} ctrl={ctrl} setCtrl={setCtrl} /> : null}
    </div>
  );
}
