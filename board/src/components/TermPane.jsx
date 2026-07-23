import React, { useEffect, useRef, useState } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { wsUrl } from '../token.js';
import { pasteImage } from '../api.js';
import { imageFromClipboard } from '../util.js';

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

// ESC CR — what Claude Code's own /terminal-setup asks a terminal to send for
// Shift+Enter, so it is what its TUI listens for as "newline, do not submit".
// Built from the char code on purpose: an ESC written literally into source is
// an invisible control character, and the next person to read this file
// deserves better.
const NEWLINE_SEQ = String.fromCharCode(27) + '\r';

function cssVar(name, fallback) {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v || fallback;
}

// xterm theme from the board's live tokens (dark or light — read at mount).
// in-file only — the effect below is its single consumer.
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
  // Transient, self-dismissing status for an image paste — its own channel, NOT
  // `note` (which means "the terminal ended"). Without this the feature is 100%
  // silent: a too-large screenshot looks identical to a successful one.
  // {kind:'busy'|'ok'|'err', text}
  const [pasteStatus, setPasteStatus] = useState(null);
  const pasteTimer = useRef(0);
  // mirror `note` into a ref for the socket effect's "already ended?" check.
  // Kept in an effect (not assigned during render) so render stays pure.
  const noteRef = useRef(null);

  useEffect(() => { noteRef.current = note; onNote?.(note); }, [note]); // eslint-disable-line react-hooks/exhaustive-deps

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
        // Belt-and-suspenders for a new board talking to an old daemon: a
        // "pane gone / spawn not live" reason IS the agent ending, so render it
        // with the calm exit styling rather than the alarming "viewer refused".
        // A current daemon already sends these as {t:'exit'} (see http.mjs).
        if (/pane (not found|is gone)|spawn is not live/.test(f.reason || '')) {
          end('exit', `agent ended — ${f.reason}`);
        } else {
          end('err', `viewer refused: ${f.reason || 'unknown'}`);
        }
      }
    };
    ws.onclose = () => { if (!st.done) end('close', 'connection closed'); };

    const sendIn = (data) => {
      if (st.done || term.options.disableStdin || ws.readyState !== WebSocket.OPEN) return false;
      ws.send(JSON.stringify({ t: 'in', data }));
      return true;
    };

    // keystrokes → agent, verbatim. xterm suppresses onData entirely while
    // disableStdin is set, so a non-live tile cannot reach the wire at all.
    const dataSub = term.onData(sendIn);

    // Shift/Ctrl/Alt+Enter → a NEWLINE, not a submit.
    //
    // A terminal cannot tell Shift+Enter from Enter: both are just CR on the
    // wire, which is why multi-line input in a normal terminal needs
    // `/terminal-setup` to teach the emulator a distinct sequence. Claude Code
    // asks for ESC CR — verbatim, from its own VS Code keybinding:
    //
    //   {key:"shift+enter", command:"workbench.action.terminal.sendSequence",
    //    args:{text:"\x1B\r"}, when:"terminalFocus"}
    //
    // Here there is nothing to configure: the board IS the emulator, so it just
    // sends those bytes itself. Plain Enter still submits, exactly as it does in
    // the terminal — this only claims the modified chords, which xterm would
    // otherwise collapse into a bare CR and submit on you mid-sentence.
    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== 'keydown' || e.key !== 'Enter' || e.metaKey) return true;
      if (!(e.shiftKey || e.ctrlKey || e.altKey)) return true; // bare Enter: submit, as always
      e.preventDefault();
      sendIn(NEWLINE_SEQ);
      return false; // and never let xterm send its own CR after ours
    });
    // Ctrl+V of an IMAGE — the one paste xterm cannot deliver. A terminal
    // connection carries text, so xterm's own paste path reads text/plain and
    // silently drops image blobs; and even locally Claude Code has no Linux
    // clipboard-image read. So the board does what a terminal cannot: lift the
    // blob off the clipboard, POST it to the daemon (which writes it under
    // FLEETDECK_HOME and answers with the path), then TYPE that path into
    // the pane — Claude Code ingests image paths everywhere. The user still
    // presses Enter: keystrokes into a live agent are irreversible, so a paste
    // must never submit on its own.
    //
    // Capture phase, so this runs before xterm's paste listener on the hidden
    // textarea inside screenRef. A clipboard with no image falls through
    // untouched — plain text paste behaves exactly as it always has. Routing
    // the typed path through sendIn keeps the grid's one-tile-types discipline:
    // a non-live tile refuses at the same gate a keystroke would (and we skip
    // the upload too — no point shipping bytes nothing may type).
    // Flash a transient status over the pane and auto-clear it. 'busy' persists
    // (it is superseded by ok/err); ok/err self-dismiss.
    const flash = (kind, text) => {
      clearTimeout(pasteTimer.current);
      setPasteStatus({ kind, text });
      if (kind !== 'busy') pasteTimer.current = setTimeout(() => setPasteStatus(null), 4000);
    };

    const onPaste = (e) => {
      const item = imageFromClipboard(e.clipboardData?.items);
      if (!item) return; // text paste — xterm's own handler takes it from here
      e.preventDefault();
      e.stopPropagation();
      if (st.done || term.options.disableStdin) return; // non-live tile: refuse before uploading
      const file = item.getAsFile();
      if (!file) { flash('err', 'could not read the pasted image'); return; }
      flash('busy', 'uploading image…');
      const reader = new FileReader();
      reader.onerror = () => flash('err', 'could not read the pasted image');
      reader.onload = () => {
        // Focus/liveness can change while the blob reads and uploads. Re-check
        // HERE so we do not ship bytes for a pane that can no longer type them —
        // and sendIn re-checks again at type time, so the path can only ever
        // reach THIS pane or be dropped, never another agent.
        if (st.done || term.options.disableStdin) { clearTimeout(pasteTimer.current); setPasteStatus(null); return; }
        pasteImage(reader.result)
          .then((res) => {
            if (res.ok && res.json?.path) {
              if (sendIn(res.json.path + ' ')) flash('ok', 'image added — press Enter to send');
              else flash('err', 'pane lost focus — paste discarded');
            } else if (res.status === 413) {
              flash('err', 'image too large (max 10 MB)');
            } else {
              flash('err', `paste failed — ${res.reason || `error ${res.status}`}`);
            }
          })
          .catch(() => flash('err', 'paste failed — daemon unreachable'));
      };
      reader.readAsDataURL(file);
    };
    const screenEl = screenRef.current;
    screenEl.addEventListener('paste', onPaste, true);

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
      screenEl.removeEventListener('paste', onPaste, true);
      clearTimeout(pasteTimer.current);
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

  // Fragment, not a wrapper: `.fd-termscreen` must stay a direct child of its
  // (position:relative) parent so its inset-based absolute sizing is unchanged,
  // and the status pill anchors to that same parent.
  return (
    <>
      <div className="fd-termscreen" ref={screenRef} />
      {pasteStatus && (
        <div className={`fd-pastestatus ${pasteStatus.kind}`} role="status">
          {pasteStatus.kind === 'busy' ? '⬆' : pasteStatus.kind === 'ok' ? '✓' : '⚠'} {pasteStatus.text}
        </div>
      )}
    </>
  );
}
