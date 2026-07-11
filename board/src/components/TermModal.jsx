import React, { useEffect, useRef, useState } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { wsUrl } from '../token.js';

// Live terminal onto a board-owned pane (v1.4). Speaks the daemon's /ws/term
// JSON-frame contract:
//   server → {t:'init', cols, rows, screen} · {t:'out', data} · {t:'exit', reason} · {t:'err', reason}
//   client → {t:'in', data} · {t:'resize', cols, rows}
//
// EVERY key goes to the agent — Esc included (Claude's TUI needs it), so the
// modal stops keydown propagation and App's global shortcuts (Esc-to-close,
// j/k/c/y/n…) never fire while it is open. Closing is the ✕ button only.

function cssVar(name, fallback) {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v || fallback;
}

// xterm theme from the board's live tokens (dark or light — read at mount).
function boardTermTheme() {
  const bg = cssVar('--code', '#0A0D13');
  const text = cssVar('--text', '#E7ECF5');
  const act = cssVar('--act', '#F0A63C');
  const alpha = (hex, a) => (/^#[0-9a-fA-F]{6}$/.test(hex) ? hex + a : hex);
  return {
    background: bg,
    foreground: text,
    cursor: act,
    cursorAccent: bg,
    selectionBackground: alpha(act, '44'),
    black: cssVar('--bg', '#0B0E14'),
    red: cssVar('--hazard', '#FF6B54'),
    green: cssVar('--ok', '#57B98A'),
    yellow: act,
    blue: cssVar('--m-comet', '#8FBCF7'),
    magenta: cssVar('--m-fable', '#C4B0FF'),
    cyan: cssVar('--m-quill', '#6FD6CD'),
    white: text,
    brightBlack: cssVar('--faint', '#5C6880'),
    brightRed: '#FF8A76',
    brightGreen: '#7BD3A6',
    brightYellow: '#F6BE6A',
    brightBlue: '#AECFFA',
    brightMagenta: '#D8CBFF',
    brightCyan: '#9AE4DD',
    brightWhite: '#FFFFFF',
  };
}

export default function TermModal({ spawnId, callsign, tmuxWindow, onClose }) {
  const screenRef = useRef(null);
  // null | {kind:'exit'|'err'|'close', text} — non-destructive: the terminal
  // stays on screen, frozen, under the strip.
  const [note, setNote] = useState(null);

  useEffect(() => {
    const term = new Terminal({
      cursorBlink: true,
      fontFamily: cssVar('--font-data', "'IBM Plex Mono', monospace"),
      fontSize: 13,
      scrollback: 5000,
      theme: boardTermTheme(),
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(screenRef.current);
    try { fit.fit(); } catch { /* container not measurable yet — init frame corrects */ }
    term.focus();

    // fitted dims travel in the URL so the daemon sizes the pane before init;
    // in LAN mode wsUrl() adds ?t=<token> — a WS handshake takes no headers
    const ws = new WebSocket(
      wsUrl('/ws/term', { spawn: spawnId, cols: term.cols, rows: term.rows }),
    );
    const st = { done: false, size: { cols: term.cols, rows: term.rows } };

    const end = (kind, text) => {
      if (st.done) return;
      st.done = true;
      setNote({ kind, text });
      term.options.disableStdin = true;
      term.options.cursorBlink = false;
      try { ws.close(); } catch { /* already closing */ }
    };

    ws.onmessage = (e) => {
      let f;
      try { f = JSON.parse(e.data); } catch { return; /* malformed frame */ }
      if (f.t === 'init') {
        // the server's size is truth for the screen it sends — resize BEFORE
        // writing so the ANSI snapshot lays out correctly (st.size first, so
        // the onResize hook below doesn't echo it back as a resize frame)
        if (f.cols && f.rows && (f.cols !== term.cols || f.rows !== term.rows)) {
          st.size = { cols: f.cols, rows: f.rows };
          term.resize(f.cols, f.rows);
        }
        // Blank slate before the seed: reconnecting into a terminal that still
        // holds a previous session's cells would interleave two screens.
        term.reset();
        if (f.screen) term.write(f.screen);
      } else if (f.t === 'out') {
        term.write(f.data ?? '');
      } else if (f.t === 'exit') {
        end('exit', `agent ended — ${f.reason || 'pane closed'}`);
      } else if (f.t === 'err') {
        end('err', `viewer refused: ${f.reason || 'unknown'}`);
      }
    };
    ws.onclose = () => { if (!st.done) end('close', 'connection closed'); };

    // keystrokes → agent, verbatim
    const dataSub = term.onData((data) => {
      if (!st.done && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ t: 'in', data }));
      }
    });
    // fit()/init resizes land here; only genuine changes go up the wire
    const resizeSub = term.onResize(({ cols, rows }) => {
      if (cols === st.size.cols && rows === st.size.rows) return;
      st.size = { cols, rows };
      if (!st.done && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ t: 'resize', cols, rows }));
      }
    });

    // window resize / modal resize → refit (coalesced to one fit per frame)
    let raf = 0;
    const refit = () => {
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(() => { try { fit.fit(); } catch { /* hidden */ } });
    };
    const ro = new ResizeObserver(refit);
    ro.observe(screenRef.current);
    window.addEventListener('resize', refit);

    return () => {
      st.done = true;
      ro.disconnect();
      window.removeEventListener('resize', refit);
      cancelAnimationFrame(raf);
      dataSub.dispose();
      resizeSub.dispose();
      try { ws.close(); } catch { /* unmounting */ }
      term.dispose();
    };
  }, [spawnId]);

  return (
    // no backdrop-click close, no Esc close — Esc belongs to the agent's TUI.
    // stopPropagation shields App's window-level shortcuts while typing here.
    <div className="fd-termwrap" role="presentation" onKeyDown={(e) => e.stopPropagation()}>
      <div className="fd-term" role="dialog" aria-label={`Live terminal ${callsign || spawnId}`}>
        <div className="fd-termhead">
          <span className="callsign">{callsign || spawnId}</span>
          {tmuxWindow && <span className="fd-panechip">⌗ {tmuxWindow}</span>}
          <span className="fd-termhint">⌨ keystrokes go to the live agent — Esc included · close with ✕</span>
          <span className="fd-spacer" />
          <button type="button" className="fd-x" aria-label="Close terminal" onClick={onClose}>✕</button>
        </div>
        <div className="fd-termbody">
          <div className="fd-termscreen" ref={screenRef} />
          {note && (
            <div className={`fd-termnote ${note.kind}`}>
              <span className="msg">{note.kind === 'err' ? '✗' : '⏻'} {note.text}</span>
              <button type="button" className="fd-ghostbtn" onClick={onClose}>Close</button>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
