import React, { useEffect, useRef, useState } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { wsUrl } from '../token.js';

// One live terminal onto one board-owned pane — the screen and the socket, with
// no chrome around it. The full-size modal (TermModal) and each tile of the grid
// (TermGrid) are both just this, in a differently-shaped box.
//
// Speaks the daemon's /ws/term JSON-frame contract:
//   server → {t:'init', cols, rows, screen} · {t:'out', data} · {t:'exit', reason} · {t:'err', reason}
//   client → {t:'in', data} · {t:'resize', cols, rows}
//
// `live` is what makes a grid possible. Every open pane STREAMS, but only the
// one the human is focused on may TYPE: keystrokes are irreversible and land in
// a real agent's TUI, so a tile you have merely glanced at must not be able to
// receive them. A non-live pane sets xterm's own disableStdin, so there is no
// path from a keypress to the wire — not a check we could forget to make.

function cssVar(name, fallback) {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v || fallback;
}

// xterm theme from the board's live tokens (dark or light — read at mount).
export function boardTermTheme() {
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

/**
 * @param spawnId  which board-owned pane to attach to
 * @param live     may this pane receive keystrokes? (exactly one tile at a time)
 * @param fontSize xterm font size — tiles run smaller than the modal
 * @param onNote   (note|null) => void — lifts {kind,text} so the frame can render it
 */
export default function TermPane({ spawnId, live = true, fontSize = 13, onNote }) {
  const screenRef = useRef(null);
  const termRef = useRef(null);
  // null | {kind:'exit'|'err'|'close', text} — non-destructive: the terminal
  // stays on screen, frozen, under the strip.
  const [note, setNote] = useState(null);
  const noteRef = useRef(null);
  noteRef.current = note;

  useEffect(() => { onNote?.(note); }, [note]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    const term = new Terminal({
      cursorBlink: live,
      disableStdin: !live,
      fontFamily: cssVar('--font-data', "'IBM Plex Mono', monospace"),
      fontSize,
      scrollback: 5000,
      theme: boardTermTheme(),
    });
    termRef.current = term;
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(screenRef.current);
    try { fit.fit(); } catch { /* container not measurable yet — init frame corrects */ }
    if (live) term.focus();

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

    // keystrokes → agent, verbatim. xterm suppresses onData entirely while
    // disableStdin is set, so a non-live tile cannot reach the wire at all.
    const dataSub = term.onData((data) => {
      if (!st.done && !term.options.disableStdin && ws.readyState === WebSocket.OPEN) {
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

    // window resize / tile resize → refit (coalesced to one fit per frame)
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
      termRef.current = null;
      ro.disconnect();
      window.removeEventListener('resize', refit);
      cancelAnimationFrame(raf);
      dataSub.dispose();
      resizeSub.dispose();
      try { ws.close(); } catch { /* unmounting */ }
      term.dispose();
    };
    // `live` is deliberately NOT a dependency: focusing a tile must not tear the
    // socket down and re-seed the screen. It is applied to the live Terminal by
    // the effect below instead.
  }, [spawnId]); // eslint-disable-line react-hooks/exhaustive-deps

  // Focus changes flip stdin on the existing terminal, in place.
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;
    const dead = !!noteRef.current; // an ended pane never types again
    term.options.disableStdin = !live || dead;
    term.options.cursorBlink = live && !dead;
    if (live && !dead) term.focus();
  }, [live]);

  return <div className="fd-termscreen" ref={screenRef} />;
}
