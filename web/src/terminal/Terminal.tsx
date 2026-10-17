import type { ITheme } from '@xterm/xterm';
import { useEffect, useRef, useState } from 'react';
import type { ShellActions } from '../shell/shell';
import { KeyBar, ctrlByte } from './KeyBar';
import { oscCopyText } from './osc';
import { insertText, newUploadId, prepareImage, toBase64 } from './paste';

type Props = {
  readonly station: string;
  readonly shellId: string;
  /** A TUI tab: Shift+Enter sends a newline, not a submit. Missing means a shell. */
  readonly kind?: 'shell' | 'tui';
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

/** The line over the terminal while an image is pasted (DL-147). */
export type PasteNoteState = { readonly tone: 'busy' | 'error'; readonly text: string };

const PASTE_ERROR_MS = 6000;

/** The characters of an OSC 52 text that the ask bar shows before it cuts the preview. */
const COPY_PREVIEW_CHARS = 120;

/** The one line preview of a text: control characters are spaces, a long text is cut. */
export function copyPreview(text: string): string {
  const flat = text.slice(0, COPY_PREVIEW_CHARS).replace(/[\u0000-\u001f\u007f]/g, ' ');
  return text.length > COPY_PREVIEW_CHARS ? `${flat}…` : flat;
}

/** The bar over the terminal while a program asks to copy text (NV-06). The person decides. */
export function CopyAsk({
  text,
  onDone,
}: {
  readonly text: string | null;
  /** The person chose: the bar goes away. */
  readonly onDone: () => void;
}) {
  if (text === null) return null;
  return (
    <div className="fd-term-copy" role="status" aria-live="polite">
      <p>
        The program in this terminal asks to copy {text.length.toLocaleString()} characters to your
        clipboard.
      </p>
      <p className="fd-term-copy-preview">{copyPreview(text)}</p>
      <div className="fd-term-actions">
        <button
          type="button"
          onClick={() => {
            void navigator.clipboard?.writeText(text)?.catch(() => undefined);
            onDone();
          }}
        >
          Copy
        </button>
        <button type="button" onClick={onDone}>
          Ignore
        </button>
      </div>
    </div>
  );
}

/** The note when the key bar cannot read the clipboard. */
const PASTE_BLOCKED = 'Paste is blocked by the browser. Long-press the terminal to paste.';

/** The first image of a paste, or null when the paste holds no image file. */
function pastedImage(event: ClipboardEvent): File | null {
  for (const item of Array.from(event.clipboardData?.items ?? [])) {
    if (item.kind === 'file' && item.type.startsWith('image/')) return item.getAsFile();
  }
  return null;
}

export function PasteNote({ note }: { readonly note: PasteNoteState | null }) {
  if (note === null) return null;
  return (
    <div
      className="fd-term-note"
      data-tone={note.tone}
      role={note.tone === 'error' ? 'alert' : 'status'}
    >
      {note.text}
    </div>
  );
}

const RESIZE_DEBOUNCE_MS = 150;

const FONT_LOAD_MS = 1500;

/** The 16 ANSI colors and the chrome of the terminal, from the board's tokens (as v1). */
export function termTheme(
  css: Pick<CSSStyleDeclaration, 'getPropertyValue'> = getComputedStyle(document.documentElement),
): ITheme {
  const v = (name: string) => css.getPropertyValue(name).trim();
  const out: Record<string, string> = {};
  const set = (key: keyof ITheme, value: string) => {
    if (value !== '') out[key] = value;
  };
  const code = v('--code');
  const text = v('--text');
  const act = v('--act');
  set('background', code);
  set('foreground', text);
  set('cursor', act);
  set('cursorAccent', code);
  set(
    'selectionBackground',
    v('--term-selection') || (/^#[0-9a-fA-F]{6}$/.test(act) ? `${act}44` : act),
  );
  set('black', v('--bg'));
  set('red', v('--hazard'));
  set('green', v('--ok'));
  set('yellow', act);
  set('blue', v('--m-comet'));
  set('magenta', v('--m-fable'));
  set('cyan', v('--m-quill'));
  set('white', text);
  set('brightBlack', v('--faint'));
  set('brightRed', '#FF8A76');
  set('brightGreen', '#7BD3A6');
  set('brightYellow', '#F6BE6A');
  set('brightBlue', '#AECFFA');
  set('brightMagenta', '#D8CBFF');
  set('brightCyan', '#9AE4DD');
  set('brightWhite', '#FFFFFF');
  return out as ITheme;
}

/** Wait for the terminal font, so xterm measures it and not a fallback; never longer than FONT_LOAD_MS. */
async function loadFont(fontFamily: string): Promise<void> {
  if (fontFamily === '' || typeof document === 'undefined' || !document.fonts) return;
  try {
    await Promise.race([
      document.fonts.load(`13px ${fontFamily}`),
      new Promise<void>((resolve) => setTimeout(resolve, FONT_LOAD_MS)),
    ]);
  } catch {
    // The font stays a fallback; the fit after `fonts.ready` corrects the metrics.
  }
}

/** An xterm.js view of one shell stream: binary frames both ways (PLAN §6.8). */
export function Terminal({ station, shellId, kind, actions, active, touch, onEnded }: Props) {
  const host = useRef<HTMLDivElement>(null);
  const [status, setStatus] = useState<Status>('connecting');
  const [attempt, setAttempt] = useState(0);
  const [ctrl, setCtrlState] = useState(false);
  const [note, setNote] = useState<PasteNoteState | null>(null);
  const [copyAsk, setCopyAsk] = useState<string | null>(null);
  const ctrlRef = useRef(false);
  const activeRef = useRef(active);
  const handles = useRef<Handles | null>(null);
  const sender = useRef<(bytes: string) => void>(() => undefined);
  const clipboardPaste = useRef<() => void>(() => undefined);
  const touchRef = useRef(touch);
  activeRef.current = active;
  touchRef.current = touch;
  const setCtrl = (on: boolean) => {
    ctrlRef.current = on;
    setCtrlState(on);
  };

  useEffect(() => {
    let disposed = false;
    let cleanup = () => {};
    setStatus('connecting');
    setNote(null);
    setCopyAsk(null);
    void (async () => {
      const [{ Terminal: XTerm }, { FitAddon }] = await Promise.all([
        import('@xterm/xterm'),
        import('@xterm/addon-fit'),
      ]);
      const el = host.current;
      if (disposed || el === null) return;
      const css = getComputedStyle(document.documentElement);
      const fontFamily = css.getPropertyValue('--font-data').trim();
      await loadFont(fontFamily);
      if (disposed) return;
      const term = new XTerm({
        cursorBlink: true,
        fontSize: touch ? 12 : 13,
        fontWeight: 400,
        fontWeightBold: 600,
        lineHeight: 1.15,
        letterSpacing: 0,
        scrollback: 5000,
        // The agent's TUI reports the mouse, so a plain drag goes to the agent. A Mac needs
        // Option+click to force a local selection.
        macOptionClickForcesSelection: true,
        allowProposedApi: false,
        ...(fontFamily === '' ? {} : { fontFamily }),
        theme: termTheme(),
      });
      const fitAddon = new FitAddon();
      term.loadAddon(fitAddon);
      const screen = document.createElement('div');
      screen.className = 'fd-term-screen';
      el.appendChild(screen);
      term.open(screen);
      const fit = () => {
        try {
          fitAddon.fit();
        } catch {
          // A hidden container has no size; the next fit (on activation) works.
        }
      };
      handles.current = { fit, focus: () => term.focus() };
      // A font that loads after the wait changes the cell size: fit again and repaint.
      void document.fonts?.ready.then(() => {
        if (disposed) return;
        fit();
        term.refresh(0, term.rows - 1);
      });
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
      // Ctrl+Shift+C and Cmd+C copy the selection. In a TUI tab Shift+Enter sends
      // ESC CR, which Claude Code's TUI reads as "newline, do not submit".
      term.attachCustomKeyEventHandler((e) => {
        if (e.type !== 'keydown') return true;
        const key = e.key.toLowerCase();
        const copy =
          (e.ctrlKey && e.shiftKey && !e.altKey && !e.metaKey && key === 'c') ||
          (e.metaKey &&
            !e.ctrlKey &&
            !e.altKey &&
            !e.shiftKey &&
            key === 'c' &&
            term.hasSelection());
        if (copy) {
          const selection = term.getSelection();
          if (selection !== '')
            void navigator.clipboard?.writeText(selection)?.catch(() => undefined);
          return false;
        }
        if (
          kind === 'tui' &&
          e.key === 'Enter' &&
          e.shiftKey &&
          !e.ctrlKey &&
          !e.altKey &&
          !e.metaKey
        ) {
          write('\x1b\r');
          return false;
        }
        return true;
      });
      // OSC 52: the program asks to copy; the person decides (NV-06). A read request gets no
      // answer. A new request replaces an older one that is still shown.
      const osc = term.parser.registerOscHandler(52, (data) => {
        const text = oscCopyText(data);
        if (text !== null && !disposed) setCopyAsk(text);
        return true;
      });
      const onTheme = () => {
        term.options.theme = termTheme();
      };
      window.addEventListener('fd:theme', onTheme);
      const input = term.onData((d) => {
        if (ctrlRef.current && d.length === 1) {
          setCtrl(false);
          write(ctrlByte(d));
        } else write(d);
      });
      // An image on the clipboard goes to the station; its path is typed (never Enter).
      let inflight = 0;
      let noteTimer: ReturnType<typeof setTimeout> | undefined;
      const flash = (text: string) => {
        if (disposed) return;
        if (noteTimer !== undefined) clearTimeout(noteTimer);
        setNote({ tone: 'error', text });
        noteTimer = setTimeout(() => setNote(null), PASTE_ERROR_MS);
      };
      const fail = (reason: string) => flash(`Image not pasted: ${reason}`);
      const pasteImage = async (file: Blob) => {
        if (noteTimer !== undefined) clearTimeout(noteTimer);
        inflight += 1;
        setNote({ tone: 'busy', text: 'Pasting image…' });
        try {
          const image = await prepareImage(file);
          if ('error' in image) {
            fail(image.error);
            return;
          }
          const path = await actions.paste(station, {
            uploadId: newUploadId(),
            ext: image.ext,
            dataBase64: toBase64(image.bytes),
          });
          const text = insertText(path);
          if (text === null) {
            fail('The station gave a path that the board cannot type.');
            return;
          }
          if (disposed) return;
          term.paste(text);
          if (inflight === 1) setNote(null);
        } catch (error) {
          fail(error instanceof Error ? error.message : 'The request failed.');
        } finally {
          inflight -= 1;
        }
      };
      const onPaste = (event: ClipboardEvent) => {
        const file = pastedImage(event);
        if (file === null) return;
        event.preventDefault();
        event.stopPropagation();
        void pasteImage(file);
      };
      el.addEventListener('paste', onPaste, true);
      // The Paste key of the key bar: the clipboard text goes in as a paste event does.
      clipboardPaste.current = () => {
        const clipboard = navigator.clipboard;
        if (typeof clipboard?.readText !== 'function') {
          flash(PASTE_BLOCKED);
          return;
        }
        clipboard.readText().then(
          (text) => {
            if (disposed) return;
            if (text !== '') term.paste(text);
            term.focus();
          },
          () => flash(PASTE_BLOCKED),
        );
      };
      // A finger swipe scrolls the scrollback of the normal buffer; a TUI (alternate buffer) keeps
      // its touches.
      let touchY: number | null = null;
      let touchRows = 0;
      const onTouchStart = (e: TouchEvent) => {
        if (!touchRef.current) return;
        touchY = e.touches.length === 1 ? (e.touches[0]?.clientY ?? null) : null;
        touchRows = 0;
      };
      const onTouchMove = (e: TouchEvent) => {
        const y = e.touches[0]?.clientY;
        if (!touchRef.current || touchY === null || e.touches.length !== 1 || y === undefined)
          return;
        const dy = y - touchY;
        touchY = y;
        if (term.buffer.active.type !== 'normal') return;
        const cellHeight = el.clientHeight / term.rows;
        if (!(cellHeight > 0)) return;
        e.preventDefault();
        touchRows += -dy / cellHeight;
        const lines = Math.trunc(touchRows);
        if (lines === 0) return;
        touchRows -= lines;
        term.scrollLines(lines);
      };
      el.addEventListener('touchstart', onTouchStart, { passive: true });
      el.addEventListener('touchmove', onTouchMove, { passive: false });
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
        el.removeEventListener('paste', onPaste, true);
        el.removeEventListener('touchstart', onTouchStart);
        el.removeEventListener('touchmove', onTouchMove);
        clipboardPaste.current = () => undefined;
        if (noteTimer !== undefined) clearTimeout(noteTimer);
        input.dispose();
        osc.dispose();
        window.removeEventListener('fd:theme', onTheme);
        resized.dispose();
        ws.onclose = null;
        ws.close();
        term.dispose();
        screen.remove();
        handles.current = null;
        sender.current = () => undefined;
      };
    })();
    return () => {
      disposed = true;
      cleanup();
    };
  }, [station, shellId, kind, actions, attempt]);

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
      <CopyAsk text={copyAsk} onDone={() => setCopyAsk(null)} />
      <div className="fd-term-host" ref={host} />
      <PasteNote note={note} />
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
      {touch ? (
        <KeyBar
          send={send}
          ctrl={ctrl}
          setCtrl={setCtrl}
          onPaste={() => clipboardPaste.current()}
        />
      ) : null}
    </div>
  );
}
