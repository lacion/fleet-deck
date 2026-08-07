import React, { useEffect, useRef, useState } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { ClipboardAddon } from '@xterm/addon-clipboard';
import '@xterm/xterm/css/xterm.css';
import { hasToken, wsUrl } from '../token.js';
import { pasteImage, fetchHealth } from '../api.js';
import { MAX_RECONNECT, reconnectPlan, refusedUpgradeText } from '../termDiag.js';
import { copyText, imageFromClipboard, isMacUA, isTermCopyChord, isTermPasteChord, pasteTextSafe, termChordHints, unwrapTmuxPassthrough } from '../util.js';

// One live terminal onto one board-owned pane — the screen and the socket, with
// no chrome around it. The floating window (TermWindow) and each tile of the grid
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

// Read once, not per keystroke: the copy chord is checked on EVERY key that
// reaches a pane.
const IS_MAC = isMacUA();

// What a successful copy says. Only the Ctrl+C platforms need the second half:
// there, the chord we just intercepted is also the agent's interrupt, and the
// human has to know it went back to being one. ⌘C never was.
const COPY_FLASH = IS_MAC
  ? 'copied'
  : 'copied — selection cleared, so the next Ctrl+C interrupts';

// Taught at the moment the gesture fails, not in a header nobody reads: the
// agent's TUI owns a plain drag, so one that selected nothing gets an answer.
const SELECT_HINT = `${termChordHints(IS_MAC).select} to select — the agent owns a plain drag`;

// How far a press must travel before we call it a drag rather than a click. A
// click is a real thing to send a mouse-mode TUI; a 24px sweep is someone
// trying to select text.
const DRAG_SLOP = 24;

// OSC 52 — THE COPY THE AGENT ITSELF PERFORMS.
//
// This is how the human actually copies out of a pane, and the board threw it
// on the floor for its entire existence. Claude Code's TUI owns the mouse
// (mouse reporting is on), so a drag never reaches xterm as a selection: the
// TUI does its OWN selection, prints its own "copied N characters", and writes
// the clipboard with OSC 52 — wrapped for tmux passthrough, which tmux forwards
// and the daemon relays verbatim. Verified on 2026-07-25: the sequence arrives
// at the viewer intact and xterm parsed it into nothing, because xterm's OSC
// table has no 52 (0,1,2,4,8,10,11,12,104,110,111,112 — and that is all).
//
// So the agent said "copied", the board's own chord said "copied", and neither
// had put anything anywhere. This addon closes it.
//
// WRITE ONLY, DELIBERATELY. OSC 52 has a READ form (`ESC ] 52 ; c ; ? BEL`)
// that answers by TYPING the clipboard back into the pane as if the human had
// pasted it. Any byte stream a pane renders can ask for that — a `cat` of a
// hostile file, a fetched page, a tool result — so honouring it would let
// anything on screen exfiltrate the operator's clipboard into a live agent's
// stdin. readText therefore returns nothing, always. The one-way trade is the
// whole point: the fleet may hand you text, never take it.
// One hard ceiling on what an OSC 52 may put on the operator's clipboard.
// xterm parses the sequence before the provider sees it, so this cannot be
// attacked with an unterminated BEL-less stream — but a pane renders bytes
// from files, tools and the network, and none of them needs a megabyte of
// clipboard. 64 KiB is past any legitimate "copied N characters".
const OSC52_MAX = 64 * 1024;

// THE FOCUS GATE, and why one exists at all. Every mounted tile loads this
// addon, but only ONE tile is live — the human's. An unfocused (watch-only)
// tile still streams output, and any byte in that stream can carry OSC 52:
// a background agent, a file it cat'd, a fetched page. Honouring those writes
// would let anything on any screen silently replace the operator's clipboard
// with attacker-chosen text — commands, URLs — that may later be pasted
// somewhere trusted. So writes are honoured only while the pane's own term
// says it may type: live and un-ended. The gate reads term.options.disableStdin
// (the same flag the keystroke gate enforces) rather than the `live` prop,
// because focus flips are applied to the live Terminal in place — the effect
// that creates this provider does not re-run when they happen.
const clipboardProvider = (term) => ({
  async readText() { return ''; },
  async writeText(_selection, data) {
    if (term.options.disableStdin) return; // watch-only / ended pane: refuse
    if (data.length > OSC52_MAX) return;
    // Through the board's own copyText, not navigator.clipboard: the LAN board
    // is plain http, where navigator.clipboard does not exist, and copyText
    // owns the fallback that still works there.
    if (data) await copyText(data);
  },
});

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
  // Reconnect state. `attempt` is a socket-effect dependency: bumping it tears
  // the dead socket down and stands a fresh viewer up, which is the same path a
  // close-and-reopen takes — just without making the human do it. The count
  // lives in a ref so it survives the re-run and resets only on a real pane
  // change (a new spawn deserves a fresh budget).
  const [attempt, setAttempt] = useState(0);
  const retriesRef = useRef(0);
  useEffect(() => { retriesRef.current = 0; }, [spawnId]);

  useEffect(() => { noteRef.current = note; onNote?.(note); }, [note]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    let retryTimer = 0; // pending reconnect (cleared on unmount / pane change)
    const term = new Terminal({
      cursorBlink: live,
      disableStdin: !live,
      fontFamily: cssVar('--font-data', "'IBM Plex Mono', monospace"),
      fontSize,
      scrollback: 5000,
      // The agent's TUI enables mouse reporting, so xterm forwards a plain drag
      // to the agent instead of selecting — on every platform, Shift forces a
      // local selection instead, but on a Mac that escape hatch is ⌥ and is OFF
      // by default. Without this a Mac has NO way to select pane text at all.
      macOptionClickForcesSelection: true,
      theme: boardTermTheme(),
    });
    termRef.current = term;
    const fit = new FitAddon();
    term.loadAddon(fit);
    // The agent's own copies (OSC 52) land on the real clipboard — see the
    // provider above for why writes are honoured on the live pane only and
    // reads never are.
    term.loadAddon(new ClipboardAddon(undefined, clipboardProvider(term)));
    term.open(screenRef.current);
    try { fit.fit(); } catch { /* container not measurable yet — init frame corrects */ }
    if (live) term.focus();

    // fitted dims travel in the URL so the daemon sizes the pane before init;
    // in LAN mode wsUrl() adds ?t=<token> — a WS handshake takes no headers
    const ws = new WebSocket(
      wsUrl('/ws/term', { spawn: spawnId, cols: term.cols, rows: term.rows }),
    );
    // `seen` = did even one frame arrive? A socket that closes without ever
    // speaking was refused at the upgrade, not disconnected mid-stream, and the
    // two need different words — see the close handler.
    const st = { done: false, seen: false, carry: '', size: { cols: term.cols, rows: term.rows } };

    // Everything the pane sends passes through here on its way to the screen.
    // tmux hands us its passthrough wrappers unopened (control mode is not a
    // terminal, so tmux never does the unwrapping a real client would get), and
    // the agent's own clipboard write is inside one — see
    // unwrapTmuxPassthrough. `carry` holds a wrapper split across two frames.
    const write = (data) => {
      const { out, carry } = unwrapTmuxPassthrough(data, st.carry);
      st.carry = carry;
      return out;
    };

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
      // A frame proves the viewer is healthy again: retire the reconnect budget
      // and clear the "reconnecting…" strip, or the note would keep the pane
      // latched read-only via the focus effect's `dead` check.
      if (!st.seen && retriesRef.current) { retriesRef.current = 0; setNote(null); }
      st.seen = true;
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
        st.carry = '';
        if (f.screen) term.write(write(f.screen));
      } else if (f.t === 'out') {
        term.write(write(f.data ?? ''));
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
    // A close with no frame before it is a REFUSED UPGRADE — see termDiag.js
    // for the full diagnostic contract. The short version: the daemon destroys
    // the socket without a word, so the browser cannot tell 401 from "the
    // network died", and "no local key ⇒ you need a key" is only sound when the
    // deployment actually gates /ws/term on one. PROXY_AUTH=trust and
    // TRUST_LOOPBACK=on both authorize tokenless upgrades, and under either one
    // the missing-key sentence is a FALSE diagnosis — the real fault is the
    // proxy dropping the upgrade or the transport dying. So the daemon's own
    // /health capability (auth.term_token) is the arbiter; when /health cannot
    // be asked (old daemon, or the fetch itself failed), the historical
    // key-based inference is the safe fallback.
    ws.onclose = () => {
      if (st.done) return;
      const plan = reconnectPlan(st.seen, retriesRef.current);
      if (plan.action === 'retry') {
        // A transport blip, not an ending: heal it. The pane stays on screen
        // holding its last frame (nothing is cleared) and says so, then a fresh
        // viewer re-seeds from capture-pane. If the agent actually ended, that
        // viewer gets the exit frame and settles properly.
        retriesRef.current += 1;
        setNote({ kind: 'close', text: `connection lost — reconnecting (${retriesRef.current}/${MAX_RECONNECT})…` });
        retryTimer = setTimeout(() => setAttempt(a => a + 1), plan.delayMs);
        return;
      }
      if (plan.action === 'give-up') return end('close', 'connection closed — reconnect gave up (reopen the terminal to retry)');
      fetchHealth().then((health) => {
        if (st.done) return; // a retry/unmount already ended this pane
        end('err', refusedUpgradeText(hasToken(), health?.auth?.term_token));
      });
    };

    const sendIn = (data) => {
      if (st.done || term.options.disableStdin || ws.readyState !== WebSocket.OPEN) return false;
      ws.send(JSON.stringify({ t: 'in', data }));
      return true;
    };

    // Flash a transient status over the pane and auto-clear it. 'busy' persists
    // (it is superseded by ok/err); ok/err self-dismiss. Shared by the two
    // clipboard paths below — an image paste and a copy are both silent
    // otherwise, and a silent clipboard is indistinguishable from a broken one.
    const flash = (kind, text) => {
      clearTimeout(pasteTimer.current);
      setPasteStatus({ kind, text });
      if (kind !== 'busy') pasteTimer.current = setTimeout(() => setPasteStatus(null), 4000);
    };

    // keystrokes → agent, verbatim. xterm suppresses onData entirely while
    // disableStdin is set, so a non-live tile cannot reach the wire at all.
    const dataSub = term.onData(sendIn);

    // Copy the selection to the system clipboard. Returns false when there is
    // nothing selected, which is the signal to let the keystroke through as an
    // ordinary interrupt.
    //
    // copyText, not the browser's own copy: xterm cancels the Ctrl+C keydown
    // before a `copy` event can fire, and on the LAN board (plain http, not a
    // secure context) navigator.clipboard does not exist at all — copyText owns
    // that fallback and, crucially, reports failure instead of pretending.
    //
    // The selection is CLEARED on success on purpose: Ctrl+C is the agent's
    // interrupt, and a chord that silently stops interrupting because a
    // forgotten selection is still on screen would be a trap. Copy once, then
    // the key is the interrupt again — and the flash says so.
    const copySelection = () => {
      const text = term.getSelection();
      if (!text) return false;
      copyText(text).then((ok) => {
        // The clipboard write is async and the pane can close under it — every
        // call below would then hit a disposed Terminal. termRef is nulled by
        // this effect's cleanup, so it is also the "still mine?" check.
        if (termRef.current !== term) return;
        if (ok) {
          term.clearSelection();
          flash('ok', COPY_FLASH);
        } else {
          // Do NOT clear the selection here: it is the only copy the human has
          // left, and right-click → Copy needs it to still be on screen (xterm
          // loads the selection into its textarea for the context menu).
          flash('err', 'the clipboard refused this copy — right-click → Copy instead (why: console)');
        }
        // The execCommand fallback selects a throwaway textarea, which takes the
        // keyboard off xterm; hand it back so the pane can still be typed into.
        if (!term.options.disableStdin) term.focus();
      });
      return true;
    };

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
      // Ctrl+C (⌘C on a Mac) WITH a selection: copy it, and swallow the key so
      // the agent is not interrupted by what the human meant as a copy. With no
      // selection nothing is claimed — see isTermCopyChord for why the chord
      // cannot simply be left to the browser.
      if (isTermCopyChord(e, IS_MAC) && copySelection()) {
        e.preventDefault();
        return false;
      }
      // Ctrl+V: take the chord away from xterm (which would send ^V) but leave
      // the event ALONE otherwise — no preventDefault — so the browser performs
      // its own trusted paste. xterm's paste handler then does the bracketing
      // when the pane asked for it; the capture-phase paste listener refuses
      // the multi-line case when it did not (see onPaste). See isTermPasteChord
      // for why a remote terminal must not send ^V here.
      if (isTermPasteChord(e, IS_MAC)) return false;
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
    const onPaste = (e) => {
      const item = imageFromClipboard(e.clipboardData?.items);
      if (!item) {
        // Text paste. xterm brackets it ONLY while the program in the pane has
        // enabled DEC mode 2004 — and the board cannot know it has: a fresh
        // viewer seeds its screen from capture-pane, which carries cells, not
        // terminal mode state, so this emulator comes up with
        // bracketedPasteMode false even when the agent asked for it. With the
        // mode off (or unreconstructable — same thing here) xterm sends the
        // text VERBATIM, and in a shell like dash each newline executes as it
        // arrives: a multi-line paste submits itself line by line. That case
        // is refused outright — it is the whole reason this listener exists.
        // Single-line text can never self-submit, so it falls through.
        const text = e.clipboardData?.getData('text/plain') ?? '';
        if (!pasteTextSafe(text, term.modes?.bracketedPasteMode)) {
          e.preventDefault();
          e.stopPropagation();
          flash('err', 'multi-line paste blocked — this pane did not ask for bracketed paste (a shell like dash), so pasting would run each line as it lands. Paste one line at a time.');
        }
        return;
      }
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

    // THE FAILED GESTURE TEACHES ITSELF. While the agent's TUI has mouse
    // reporting on, xterm hands it every plain drag and makes no selection —
    // so "I dragged across the text and copied nothing" is silent by
    // construction. Nothing in the pane answered it; the chord lived in a
    // header hint, which is the same as nowhere. Now a sweep that cannot
    // select says why, once per gesture, and only when a modifier would
    // actually have changed the outcome.
    let sweep = null;
    const mouseModeOn = () => (term.modes?.mouseTrackingMode ?? 'none') !== 'none';
    const onDown = (e) => {
      // A press that already carries the modifier IS the selecting gesture.
      sweep = e.button !== 0 || e.shiftKey || (IS_MAC && e.altKey)
        ? null
        : { x: e.clientX, y: e.clientY, taught: false };
    };
    const onMove = (e) => {
      if (!sweep || sweep.taught) return;
      if (Math.abs(e.clientX - sweep.x) + Math.abs(e.clientY - sweep.y) < DRAG_SLOP) return;
      sweep.taught = true;
      // Selection already works when the app is not tracking the mouse — say
      // nothing there, or the hint becomes noise over a drag that succeeded.
      if (mouseModeOn() && !term.hasSelection()) flash('hint', SELECT_HINT);
    };
    const onUp = () => { sweep = null; };
    screenEl.addEventListener('mousedown', onDown);
    screenEl.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);

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
      screenEl.removeEventListener('mousedown', onDown);
      screenEl.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
      clearTimeout(pasteTimer.current);
      clearTimeout(retryTimer);
      cancelAnimationFrame(raf);
      dataSub.dispose();
      resizeSub.dispose();
      try { ws.close(); } catch { /* unmounting */ }
      term.dispose();
    };
    // `live` is deliberately NOT a dependency: focusing a tile must not tear the
    // socket down and re-seed the screen. It is applied to the live Terminal by
    // the effect below instead. `attempt` IS one: bumping it is how a dropped
    // transport rebuilds its viewer (see ws.onclose).
  }, [spawnId, attempt]); // eslint-disable-line react-hooks/exhaustive-deps

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
          {/* the hint's icon is deliberately NEUTRAL: its text already opens with
              the modifier glyph, and that glyph is ⌥ on a Mac — a hardcoded ⇧
              here rendered "⇧ ⇧drag" on Linux and a contradiction on macOS. */}
          {pasteStatus.kind === 'busy' ? '⬆' : pasteStatus.kind === 'ok' ? '✓' : pasteStatus.kind === 'hint' ? 'ⓘ' : '⚠'} {pasteStatus.text}
        </div>
      )}
    </>
  );
}
