// Small pure helpers shared across the board.

import type { SessionEntry, TickerEntry, QuestionEntry } from '../../contracts/index.ts';

// The mutable diagnostic record copyText threads through its attempt chain; the
// event/async paths each stamp WHICH link worked (see copyText's header).
interface CopyTrace {
  at: string;
  chars: number;
  ok?: boolean;
  execCommand?: string;
  copyEvent?: string;
  writeText?: string;
  verified?: string;
  result?: string;
  secureContext?: boolean | null;
  documentFocused?: boolean | null;
}

// What copyViaEvent reports back — its three fields Object.assign onto the trace.
interface CopyAttempt {
  ok: boolean;
  execCommand: string;
  copyEvent: string;
}

// The duck-typed slice of a caught value the diagnostics read (a caught value is
// `unknown`; only these two fields are ever looked at, both optional-chained).
interface ErrLike {
  name?: string;
  message?: string;
}

// The slice of `document` copyViaEvent drives. Reached through this structural
// surface rather than the global: lib.dom types `document` as always present
// (it is undefined under `node --test`) and marks execCommand deprecated (it is
// the whole mechanism here), so the guard stays honest and the call reads clean.
interface CopyDoc {
  execCommand?: (commandId: string) => boolean;
  activeElement: Element | null;
  body: { appendChild: (node: HTMLTextAreaElement) => void };
  createElement: (tag: 'textarea') => HTMLTextAreaElement;
  addEventListener: (type: 'copy', listener: (e: ClipboardEvent) => void, capture: boolean) => void;
  removeEventListener: (
    type: 'copy',
    listener: (e: ClipboardEvent) => void,
    capture: boolean,
  ) => void;
}

// Put `text` on the clipboard, and return whether it PROVABLY got there.
//
// Reported 2026-07-25, after the first version of this shipped: the board said
// "✓ copied" and the clipboard still held something pasted from another app an
// hour earlier. The cause is the contract of the two APIs, not the browser:
//
//   navigator.clipboard.writeText() resolves when the write is ACCEPTED. A
//   resolved promise is not a changed clipboard — Chrome can and does drop the
//   write afterwards (a focus change mid-flight is the usual reason), and there
//   is no callback for that. document.execCommand('copy') is worse: it returns
//   true for "the command ran", including runs the browser silently discards.
//
// So neither return value may be believed on its own. The EVENT path can be:
// a `copy` listener fires with the real ClipboardEvent, and setData() on that
// event's clipboardData IS the write — if our listener ran, the browser is
// holding our string, not a maybe. That path goes first for exactly that
// reason, and its proof (`fired`) is what this function reports. The async API
// is kept as the fallback for the case execCommand is unavailable, where an
// unprovable true still beats refusing to copy at all.
//
// Callers must treat `false` as "tell the human to copy it themselves" — a
// silent lie is what cost three rounds of debugging here.
export async function copyText(text: string | null | undefined): Promise<boolean> {
  // Every attempt records WHICH link of the chain worked, because "it says
  // copied and my clipboard is unchanged" is otherwise undebuggable from the
  // outside: the page cannot see the OS clipboard, so the only evidence
  // available is which step reported what. Left on the window on purpose —
  // `__fdCopy` is what to ask a reporter for, and it costs one object.
  const trace: CopyTrace = { at: new Date().toISOString(), chars: text?.length ?? 0 };
  const attempt = copyViaEvent(text);
  Object.assign(trace, attempt);
  if (attempt.ok) {
    // A granted read-back OVERRULES the accepted write: the clipboard provably
    // holding something else means this copy failed, whatever the event said —
    // reporting success here would clear the human's selection over a clipboard
    // that still holds the PREVIOUS text.
    const verified = await verifyClipboard(trace, text);
    return finishCopyTrace(trace, verified !== false);
  }
  const clip = (navigator as { clipboard?: Clipboard }).clipboard;
  if (clip?.writeText) {
    try {
      await clip.writeText(text ?? '');
      trace.writeText = 'resolved';
      if ((await verifyClipboard(trace, text)) === false) return finishCopyTrace(trace, false);
      return finishCopyTrace(trace, true);
    } catch (err) {
      const e = err as ErrLike | null | undefined;
      trace.writeText = `rejected: ${(e?.name ?? '') || 'Error'} — ${e?.message ?? ''}`;
    }
  } else {
    trace.writeText = 'unavailable (not a secure context?)';
  }
  return finishCopyTrace(trace, false);
}

// Best-effort READ-BACK: the only way a page can learn whether the clipboard
// actually changed. Chrome prompts for clipboard-read, so this runs ONLY when
// the permission is already granted — a diagnostic may never put a permission
// dialog in front of someone who just pressed Ctrl+C. When it does run it
// settles the question the trace otherwise can only infer.
//
// Tri-state, and the caller treats ONLY the observed mismatch as failure:
//   true      the clipboard provably holds this text
//   false     it provably holds something else — the write was dropped
//   undefined it could not be checked (no permission, read failed) — the
//             accepted write is the best evidence there is, so it stands
async function verifyClipboard(
  trace: CopyTrace,
  expected: string | null | undefined,
): Promise<boolean | undefined> {
  try {
    const perm = await (navigator as { permissions?: Permissions }).permissions?.query({
      name: 'clipboard-read',
    } as unknown as PermissionDescriptor);
    if (perm?.state !== 'granted') {
      trace.verified = `not checked (permission: ${perm?.state ?? 'unknown'})`;
      return undefined;
    }
    const got = await navigator.clipboard.readText();
    if (got === expected) {
      trace.verified = 'YES — the clipboard holds this text';
      return true;
    }
    trace.verified = `NO — the clipboard holds something else (${got.length} chars)`;
    return false;
  } catch (err) {
    const e = err as ErrLike | null | undefined;
    trace.verified = `read failed: ${(e?.name ?? '') || 'Error'}`;
    return undefined;
  }
}

function finishCopyTrace(trace: CopyTrace, ok: boolean): boolean {
  // `ok` alone can't say which: a verified NO also lands here, and the trace
  // must distinguish "provably copied" from "accepted but uncheckable" (and
  // "refused" from "accepted, then PROVEN to have landed nowhere").
  trace.result = ok
    ? trace.verified?.startsWith('YES')
      ? 'verified as copied'
      : 'reported as copied (unverified)'
    : trace.verified?.startsWith('NO')
      ? 'refused — the clipboard holds something else'
      : 'refused';
  trace.secureContext = typeof window !== 'undefined' ? window.isSecureContext : null;
  trace.documentFocused = typeof document !== 'undefined' ? document.hasFocus() : null;
  try {
    (globalThis as { __fdCopy?: CopyTrace }).__fdCopy = trace;
  } catch {
    /* frozen global — the log below still carries it */
  }
  // A copy that could not be PROVEN is a real defect somewhere in the stack, so
  // it is loud. A proven one says nothing: the pane already shows a pill.
  if (!ok)
    console.warn('[fleetdeck] copy could not be proven — nothing was put on the clipboard', trace);
  return ok;
}

// Drive a real copy event and write the data ourselves. In-file only: copyText
// is the surface. Synchronous ON PURPOSE — it must run inside the user gesture
// that asked for the copy, and it must finish before the caller can clear the
// selection out from under it.
function copyViaEvent(text: string | null | undefined): CopyAttempt {
  const doc = (globalThis as { document?: CopyDoc }).document;
  const exec = doc?.execCommand?.bind(doc);
  if (!doc || !exec) {
    return { ok: false, execCommand: 'unavailable', copyEvent: 'not reached' };
  }
  // Held on an object, not a bare local: execCommand fires `copy` synchronously
  // and inline, but the compiler cannot follow that call into onCopy, so it
  // would read a plain local as permanently false at the return below.
  const proof = { fired: false };
  const onCopy = (e: ClipboardEvent) => {
    proof.fired = true;
    // Our text, not the document's selection — the throwaway textarea below
    // exists only to make the command legal, never to be the payload.
    e.clipboardData?.setData('text/plain', text ?? '');
    e.preventDefault();
  };
  // Capture phase: this must beat any other copy listener on the page (xterm
  // registers one on its own element and would answer with a selection we may
  // be about to clear).
  doc.addEventListener('copy', onCopy, true);
  const active = doc.activeElement;
  const ta = doc.createElement('textarea');
  try {
    // execCommand('copy') is a no-op unless something is selected, so give it
    // the smallest possible selection to act on.
    ta.value = text ?? '';
    ta.setAttribute('readonly', '');
    ta.style.position = 'fixed';
    ta.style.top = '0';
    ta.style.opacity = '0';
    doc.body.appendChild(ta);
    ta.select();
    const ran = exec('copy');
    return {
      // `fired` is the proof — see the header. `ran` alone is not.
      ok: ran && proof.fired,
      execCommand: ran ? 'returned true' : 'returned false',
      copyEvent: proof.fired ? 'fired (our text was written to the event)' : 'NEVER FIRED',
    };
  } catch (err) {
    const e = err as ErrLike | null | undefined;
    return {
      ok: false,
      execCommand: `threw: ${(e?.message ?? '') || String(err)}`,
      copyEvent: 'not reached',
    };
  } finally {
    doc.removeEventListener('copy', onCopy, true);
    ta.remove();
    // Hand the keyboard back to whoever had it — for a terminal pane that is
    // xterm's hidden textarea, and losing it would silently stop your typing.
    try {
      (active as { focus?: () => void } | null)?.focus?.();
    } catch {
      /* gone from the DOM already */
    }
  }
}

export function human(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

export function hhmmss(t: number): string {
  const d = new Date(t);
  const p = (n: number) => String(n).padStart(2, '0');
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

export function basename(p: string | null | undefined): string {
  // Untrusted at the questionView Edit/Write seam (a persisted payload's
  // file_path can be a number); coerce before lastIndexOf/slice, as pre-TS did.
  const s = String(p ?? '');
  const i = Math.max(s.lastIndexOf('/'), s.lastIndexOf('\\'));
  return i === -1 ? s : s.slice(i + 1);
}

// Defensive structural views of a /state session used by the helpers below. The
// board guards for shapes an older daemon may omit (spawn / adopt / remote), which
// the current /state contract marks as always-present — so these local types keep
// every `?.` guard honest while a real SessionEntry still flows in unchanged.
interface SpawnLike {
  spawn_id: string;
  status: string;
  stalled: boolean;
  remote?: { enabled: boolean };
}
interface AdoptLike {
  eligible: 'now' | 'arm' | null;
  armed: boolean;
  armed_until: number | null;
}
interface SessionLike {
  worktree: string | null;
  cwd: string | null;
  col: string;
  repo_name: string | null;
  spawn?: SpawnLike;
  adopt?: AdoptLike;
}

// v2.2 file viewer — an absolute ledger path (s.files) relative to the
// session's browse root, or null when it lives outside it (a /tmp scratch
// file, another repo). The root guess mirrors the daemon's resolution order
// (worktree ?? cwd); the daemon re-derives it from the session id anyway, so a
// mismatch here can only cost a click, never leak a path.
export function relToRoot(
  s: SessionLike | null | undefined,
  abs: string | null | undefined,
): string | null {
  const root = (s?.worktree ?? '') || (s?.cwd ?? '');
  const p = abs ?? '';
  if (!root || !p) return null;
  if (p === root) return '';
  return p.startsWith(root + '/') ? p.slice(root.length + 1) : null;
}

// M-S1 — `remote.url` is harvested by the daemon from the AGENT's terminal
// output, which is the least-trusted party in the whole system. It reaches the
// board and lands in window.open()/<a href>, so a `javascript:` (or `data:`)
// URL there would EXECUTE on click. Nothing legitimate lives outside claude.ai,
// so gate every use of that URL through here: it returns the URL only when it
// parses to https on claude.ai (or a subdomain), and null for everything else —
// the caller then renders the plain, non-clickable chip instead of a live link.
export function safeUrl(u: string | null | undefined): string | null {
  try {
    const url = new URL((u ?? '').trim());
    if (
      url.protocol === 'https:' &&
      (url.hostname === 'claude.ai' || url.hostname.endsWith('.claude.ai'))
    ) {
      return url.href;
    }
  } catch {
    /* not a parseable URL */
  }
  return null;
}

// Every family modelFamily() can name needs a matching .fd-mbadge.<fam> rule in
// app.css and a --m-<fam>/--m-<fam>-bg token pair in BOTH themes of tokens.css.
// board-util.test.mjs enforces that mechanically.
export const MODEL_FAMILIES = ['opus', 'sonnet', 'haiku', 'fable', 'quill', 'comet'];

// Split a model id into renderable parts, once, so the three helpers below can
// each render it their own way without re-parsing each other's output.
//
//   'claude-opus-4-8[1m]' → { parts: [{ver:false,'Opus'}, {ver:true,'4.8'}], marker: '1M' }
//
// Version digits arrive as separate '-' tokens ('opus-4-8'), so a RUN of numeric
// tokens is collapsed into one dotted version IN PLACE — that keeps 'fable-5-mini'
// reading as 'Fable 5 Mini' rather than reordering it.
// (in-file only — prettyModel/modelShort are the exported surface over it.)
interface ModelPart {
  ver: boolean;
  text: string;
}
interface ParsedModel {
  parts: ModelPart[];
  marker: string | null;
  raw: string;
}
function parseModel(model: string | null | undefined): ParsedModel | null {
  // The daemon persists the payload model raw (events.ts stores m.display_name
  // ?? m.id with no coercion — pre-TS relied on the board as the backstop), so
  // a `model: {display_name: 42}` SessionStart lands here as a number. Coerce
  // before .trim(), or prettyModel throws in the session cards — OUTSIDE the
  // question-rail ErrorBoundary — and white-screens the whole board on reload.
  let s = String(model ?? '').trim();
  if (!s) return null;

  // The long-context marker must come off BEFORE tokenizing: the build-tag
  // filter below matches '1m' and would silently eat it. It arrives bracketed
  // from the CLI ('...[1m]') and bare from our own output ('Opus 4.8 1M') —
  // accept both, so prettyModel is idempotent over what it just rendered.
  let marker: string | null = null;
  const bracketed = /\[([^\]]+)\]\s*$/.exec(s);
  const bare = /[\s\-_](\d+m)\s*$/i.exec(s);
  if (bracketed) {
    marker = (bracketed[1] ?? '').toUpperCase();
    s = s.slice(0, bracketed.index);
  } else if (bare) {
    marker = (bare[1] ?? '').toUpperCase();
    s = s.slice(0, bare.index);
  }

  const toks = s
    .replace(/^claude[-_ ]/i, '')
    .split(/[-_ .]+/) // '.' too, so prettyModel is idempotent over its own output
    .filter(Boolean)
    .filter((t) => !/^\d{6,}$/.test(t)) // 20250929 datestamps
    .filter((t) => !/^v?\d+[a-z]\d*$/i.test(t)); // build tags

  const parts: ModelPart[] = [];
  for (let i = 0; i < toks.length; i++) {
    if (/^\d+$/.test(toks[i] ?? '')) {
      const run: string[] = [];
      while (i < toks.length && /^\d+$/.test(toks[i] ?? '')) run.push(toks[i++] ?? '');
      i--;
      parts.push({ ver: true, text: run.join('.') });
    } else {
      const t = toks[i] ?? '';
      parts.push({ ver: false, text: (t[0] ?? '').toUpperCase() + t.slice(1).toLowerCase() });
    }
  }
  // Legacy ids put the version first ('claude-3-5-haiku'). Hoist it behind the
  // name so the badge never shows '3.5 Haiku' beside a 'Haiku 4.5'.
  if (parts.length > 1 && parts[0]?.ver) {
    const first = parts.shift();
    if (first) parts.push(first);
  }

  return { parts, marker, raw: String(model).trim() };
}

// 'claude-opus-4-8[1m]' → 'Opus 4.8 1M' · 'claude-3-5-haiku-20241022' → 'Haiku 3.5'
export function prettyModel(model: string | null | undefined): string {
  const p = parseModel(model);
  if (!p) return '—';
  const name = p.parts.map((x) => x.text).join(' ');
  if (!name) return p.raw;
  return p.marker ? `${name} ${p.marker}` : name;
}

// Compact cards: initials + version, no marker — 'claude-opus-4-8[1m]' → 'O4.8'.
export function modelShort(model: string | null | undefined): string {
  const p = parseModel(model);
  if (!p) return '—';
  return p.parts.map((x) => (x.ver ? x.text : (x.text[0] ?? ''))).join('') || p.raw;
}

// model family → CSS class carrying the --m-* token pair
export function modelFamily(model: string | null | undefined): string {
  // Same untrusted-model seam as parseModel: coerce before .toLowerCase().
  const m = String(model ?? '').toLowerCase();
  return MODEL_FAMILIES.find((f) => m.includes(f)) ?? 'other';
}

// ------------------------------------------------------------------ batch spawn
// In batch mode the prompt box stops being one prompt and becomes a task LIST:
// one agent per non-empty line, and an optional "3x " prefix runs that line's
// task three times (to race several attempts at the same problem).
//
//   3x fix the flaky worktree test     → 3 agents, same task
//   update the README                  → 1 agent
//
// This is only ever applied when the human explicitly ticks "batch" — a prompt
// with newlines in it is otherwise still ONE prompt, which matters enormously
// for plan execution, where the whole plan is prefilled into this box.
//
// The multiplier is capped at two digits on purpose: with no spawn cap left in
// the daemon, "300x" as a typo should be unrepresentable rather than expensive.
const BATCH_REPEAT_RE = /^(\d{1,2})\s*[x×]\s+(.+)$/i;

interface BatchTask {
  count: number;
  prompt: string;
}

export function parseBatchTasks(text: string | null | undefined): BatchTask[] {
  return (text ?? '')
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const m = BATCH_REPEAT_RE.exec(line);
      if (!m) return { count: 1, prompt: line };
      return { count: Math.max(1, parseInt(m[1] ?? '', 10)), prompt: (m[2] ?? '').trim() };
    })
    .filter((t) => t.prompt);
}

/** How many agents `parseBatchTasks` output would actually launch. */
export function batchTotal(tasks: BatchTask[] | null | undefined): number {
  return (tasks ?? []).reduce((n, t) => n + t.count, 0);
}

/** The flat prompt list, in launch order — one entry per agent. */
export function expandBatchTasks(tasks: BatchTask[] | null | undefined): string[] {
  return (tasks ?? []).flatMap((t) => Array.from({ length: t.count }, () => t.prompt));
}

// ---------------------------------------------------- floating terminal rect

interface WinRect {
  x: number;
  y: number;
  w: number;
  h: number;
}
interface WinViewport {
  w: number;
  h: number;
}

// The floating terminal window's geometry bounds. MIN is small enough for a
// glance-sized terminal, the margin keeps the drag handle reachable when a
// window is shoved off an edge (48px of header always stays on screen).
export const TERMWIN_MIN = { w: 380, h: 260 };
export const TERMWIN_EDGE = 48;

/**
 * Clamp a floating-window rect to a viewport so it is always grabbable:
 * size fits (never larger than the viewport, never below TERMWIN_MIN) and at
 * least TERMWIN_EDGE px of the window remains inside every viewport edge.
 * Pure — also sanitizes a malformed rect (NaN/negative from a stale
 * localStorage entry) to a centered default.
 */
export function clampWinRect(
  rect: Partial<WinRect> | null | undefined,
  viewport: Partial<WinViewport> | null | undefined,
): WinRect {
  const vw = Math.max((viewport?.w ?? 0) || 0, TERMWIN_MIN.w);
  const vh = Math.max((viewport?.h ?? 0) || 0, TERMWIN_MIN.h);
  const num = (v: number | undefined, fb: number): number =>
    typeof v === 'number' && Number.isFinite(v) ? v : fb;
  let w = Math.round(num(rect?.w, 1060));
  let h = Math.round(num(rect?.h, 720));
  w = Math.min(Math.max(w, TERMWIN_MIN.w), vw);
  h = Math.min(Math.max(h, TERMWIN_MIN.h), vh);
  // default position: centered
  let x = Math.round(num(rect?.x, (vw - w) / 2));
  let y = Math.round(num(rect?.y, (vh - h) / 2));
  // keep at least TERMWIN_EDGE of the window inside every edge
  x = Math.min(Math.max(x, TERMWIN_EDGE - w), vw - TERMWIN_EDGE);
  y = Math.min(Math.max(y, 0), vh - TERMWIN_EDGE); // top edge hard-stops: the drag bar must stay reachable
  return { x, y, w, h };
}

// v1.4 live terminal — a board-owned pane is viewable while the pane exists:
// spawning (incl. the stalled watchdog state) or live; never after exit/kill.
export function spawnTermable(s: SessionLike | null | undefined): boolean {
  if (!s?.spawn) return false;
  const st = s.spawn.stalled ? 'stalled' : s.spawn.status || 'live';
  return st === 'spawning' || st === 'stalled' || st === 'live';
}

// v1.8 kill-from-the-card — a board-owned pane can be killed while a window
// still exists to kill: spawning / stalled / live, and pane-dead (the dead
// pane's window survives until something kills it). 'killed' and 'gone' are
// terminal — there is nothing left to take down, so the board offers nothing.
export function spawnKillable(s: SessionLike | null | undefined): boolean {
  if (!s?.spawn?.spawn_id) return false;
  const st = s.spawn.status || 'live';
  return st !== 'killed' && st !== 'gone';
}

// v1.6 remote control — the "enable remote" door is offered only when the
// daemon would say yes: a LIVE board-owned pane (not spawning/stalled — /rc
// is typed into a working TUI), not already on remote control, and the
// session at a turn boundary (queued/idle). The daemon 409s every other col
// ("never inject into a working/needsyou turn"), so the board never offers
// what would be refused.
export function spawnRemoteAvailable(s: SessionLike | null | undefined): boolean {
  if (s?.spawn?.status !== 'live' || s.spawn.remote?.enabled) return false;
  return s.col === 'queued' || s.col === 'idle';
}

// v2.0 "Move to tmux" — the three pure gates over `s.adopt`, the object the
// snapshot attaches to a session the board could adopt into a tmux pane:
//   { eligible: 'now' | 'arm' | null, armed, armed_until, armed_skip }
// It is ABSENT (or null) on board-owned cards — they already have a pane — and
// on daemons that predate the feature, so every gate guards for that.
//
//   adoptableNow  the session ended with a hook-proven end → adopt is immediate
//   adoptArmable  the session is still live → the move must be ARMED and deferred
//   adoptArmed    a move is armed AND its deadline is still in the future
//
// adoptArmed repeats the daemon's own `armed_until > now` check client-side, so
// a stale snapshot never paints a chip as armed past its expiry (the arm is
// restart-durable but sweep-free: expiry is just the deadline lapsing).
export function adoptableNow(s: SessionLike | null | undefined): boolean {
  return s?.adopt?.eligible === 'now';
}
export function adoptArmable(s: SessionLike | null | undefined): boolean {
  return s?.adopt?.eligible === 'arm';
}
export function adoptArmed(s: SessionLike | null | undefined): boolean {
  const a = s?.adopt;
  return !!(a?.armed && a.armed_until && a.armed_until > Date.now());
}

// Daemon columns → board columns. `needsyou` renders in WORKING with amber
// treatment (the question itself lives in the rail — F1/F6: attention is
// global, the card just shows the session is blocked on you).
export const COLS = [
  { key: 'queued', label: 'QUEUED' },
  { key: 'working', label: 'WORKING' },
  { key: 'verifying', label: 'VERIFYING' },
  { key: 'idle', label: 'IDLE' },
  { key: 'offline', label: 'OFFLINE' },
];

export function boardCol(col: string): string {
  if (col === 'needsyou') return 'working';
  return COLS.some((c) => c.key === col) ? col : 'idle';
}

// The pulse-dot class for a session's column — the amber "working" glow, the
// verifying tint, the needsyou amber, the offline grey, or the resting "still".
// SessionCard's dot and the Drawer head both render this off `s.col`; one chain
// here so the two surfaces can never drift apart (D12).
export function colPulse(col: string): string {
  if (col === 'working') return 'working';
  if (col === 'verifying') return 'verifying';
  if (col === 'needsyou') return 'needsyou';
  if (col === 'offline') return 'offline';
  return 'still';
}

// A session's worktree is only worth badging when it is a REAL secondary
// worktree: the daemon records worktree = toplevel of cwd even for the main
// tree, so a worktree whose basename equals the repo name is just "main" and
// gets no chip. Returns the short worktree name to show, or null (D12: was
// inlined identically in SessionCard and the Drawer).
export function worktreeLabel(s: SessionLike): string | null {
  return s.worktree && basename(s.worktree) !== s.repo_name ? basename(s.worktree) : null;
}

// The session lookup every surface rebuilds: session_id → session. Built fresh
// from the current snapshot — sessions are replaced wholesale each frame, so
// there is nothing to memo across frames beyond what the caller already does
// (D12: App, BoardLanes and Inbox each open-coded this Map).
export function sessionsById(sessions: SessionEntry[]): Map<string, SessionEntry> {
  return new Map(sessions.map((s): [string, SessionEntry] => [s.session_id, s]));
}

// Prefer a session's callsign, falling back to the raw id when the session has
// been archived out of the snapshot — used wherever a conflict/mail names a sid
// the board may no longer be showing.
export function callsignOf(byId: Map<string, SessionEntry>, sid: string): string {
  return (byId.get(sid)?.callsign ?? '') || sid;
}

// ------------------------------------------------------------ callsign naming
// A callsign is `<animal>-<suffix>`: 'wren-a9e1' (suffix = the first 4 chars of
// the session uuid), 'wren-PROJ-123' (a ticket), or 'wren-docs-review' (a human
// name). The ANIMAL is minted by the daemon and is never chosen — only the
// SUFFIX is renameable — so the split is always at the FIRST dash: everything
// after it, dashes included, is ONE suffix ('PROJ-123' is not two fields).
export function animalOf(callsign: string | null | undefined): string {
  const s = callsign ?? '';
  const i = s.indexOf('-');
  return i === -1 ? s : s.slice(0, i);
}
export function suffixOf(callsign: string | null | undefined): string {
  const s = callsign ?? '';
  const i = s.indexOf('-');
  return i === -1 ? '' : s.slice(i + 1);
}

// The suffix charset is LOAD-BEARING — do NOT loosen it:
//   · sessionTicker (below) matches a callsign flanked by [^A-Za-z0-9-] so that
//     'raven-PROJ-1' doesn't leak into 'raven-PROJ-12's timeline; a suffix
//     carrying a space, a dot or a slash would slip straight through that guard;
//   · the pane's tmux window is named fd<port>-<callsign> and has to survive a
//     shell unquoted.
// Letters, digits and dashes only; must START alphanumeric (a leading '-' reads
// as a flag everywhere downstream); max 24 chars. The daemon validates it again
// and is the AUTHORITY — this is the client-side copy that keeps the rename
// dialog honest (inline hint, disabled confirm) before the round-trip.
const SUFFIX_RE = /^[A-Za-z0-9][A-Za-z0-9-]{0,23}$/;
// 24 = the 1 leading char + the 23 trailing ones SUFFIX_RE allows. The rename
// input's maxLength reads it from here, so the two can't drift apart.
export const SUFFIX_MAX = 24;
export function validSuffix(s: string | null | undefined): boolean {
  return SUFFIX_RE.test(s ?? '');
}

// The drawer's per-session timeline: the daemon's global ticker filtered to the
// rows that name this callsign, capped at the newest 12. The ticker carries only
// {at, msg}, so the match is by text — the same convention the feed classifier
// relies on.
//
// Boundary match, not a bare .includes(): ticket suffixes prefix-nest —
// 'raven-PROJ-1' is a substring of 'raven-PROJ-12' — where the old fixed-length
// hex suffix never could, so .includes() would leak the shorter ticket's
// timeline into every longer one. Require the callsign to be flanked by a
// non-[A-Za-z0-9-] character (or a string edge) on each side. The RegExp is
// built once per call, not once per row.
export function sessionTicker(
  ticker: TickerEntry[] | null | undefined,
  callsign: string | null | undefined,
): TickerEntry[] {
  const esc = (callsign ?? '').replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const re = new RegExp(`(^|[^A-Za-z0-9-])${esc}([^A-Za-z0-9-]|$)`);
  return (ticker ?? []).filter((e) => re.test(e.msg || '')).slice(0, 12);
}

// The one sentence the board repeats wherever a message won't be delivered
// instantly: it rides the agent's turn loop, so it lands at the next boundary.
// Quoted verbatim across Compose/Inbox/Drawer/PlanLibrary — one constant so the
// copy stays identical and changes in a single place (D12).
export const TURN_BOUNDARY_HINT = 'next turn boundary — idle sessions usually wake within seconds';

// Ticker rows are {at, msg} only — the tag is derived from the daemon's
// message conventions (derive.mjs / questions.mjs tick() calls).
export function classifyTicker(
  msg: string | null | undefined,
): 'confl' | 'ask' | 'mail' | 'join' | 'tool' {
  const m = msg ?? '';
  if (m.startsWith('⚠')) return 'confl';
  if (/^(🖐|❓|⌛|✅|💬)/.test(m)) return 'ask';
  if (/^(✉|📣|📌|📝)/.test(m)) return 'mail';
  if (/joined the fleet|left the fleet/.test(m)) return 'join';
  return 'tool';
}

export function stripTickerEmoji(msg: string | null | undefined): string {
  return (msg ?? '').replace(/^(⚠|🖐|❓|⌛|✅|💬|✉|📣|📌|📝)\s*/u, '');
}

// The choice/permission hook payload shapes questionView reads out of the raw
// (untrusted) question payload. Every field the code touches carries a string in
// the hook contract, so each is typed as such; the payload as a whole is still
// cast from `unknown` at questionView's entry rather than trusted wholesale.
interface ChoiceOption {
  label: string;
  description?: string;
}
interface ChoiceQuestion {
  question: string;
  header?: string;
  multiSelect?: boolean;
  options?: ChoiceOption[];
}
interface ToolInput {
  command?: string;
  file_path?: string;
  old_string?: string;
  new_string?: string;
  content?: string;
  questions?: ChoiceQuestion[];
}
interface ElicitSchema {
  properties?: unknown;
  required?: unknown;
}
interface QuestionPayload {
  tool_name?: string;
  tool_input?: ToolInput;
  text?: string;
  message?: string;
  requestedSchema?: ElicitSchema | null;
}
interface DiffLine {
  kind: string;
  text: string;
}
interface QuestionView {
  title: string;
  command?: string | null;
  diff?: DiffLine[];
  questions?: ChoiceQuestion[];
  schema?: ElicitSchema | null;
}

// Question title + command block per kind, from the raw hook payload.
export function questionView(q: QuestionEntry): QuestionView {
  const p = (q.payload ?? {}) as QuestionPayload;
  if (q.kind === 'permission') {
    const tool = (p.tool_name ?? '') || 'tool';
    const input: ToolInput = p.tool_input ?? {};
    if (tool === 'Bash' && input.command) {
      const cmd = String(input.command); // untrusted persisted payload: coerce before .length/.slice
      return {
        title: `Run \`${cmd.length > 60 ? cmd.slice(0, 57) + '…' : cmd}\`?`,
        command: '$ ' + cmd,
      };
    }
    if ((tool === 'Edit' || tool === 'MultiEdit') && input.file_path) {
      const olds = String(input.old_string ?? '');
      const news = String(input.new_string ?? '');
      const lines = [
        ...olds
          .split('\n')
          .filter(Boolean)
          .slice(0, 6)
          .map((l) => ({ kind: 'del', text: '- ' + l })),
        ...news
          .split('\n')
          .filter(Boolean)
          .slice(0, 6)
          .map((l) => ({ kind: 'add', text: '+ ' + l })),
      ];
      return { title: `Edit ${basename(input.file_path)}?`, command: input.file_path, diff: lines };
    }
    if (tool === 'Write' && input.file_path) {
      const body = String(input.content ?? '')
        .split('\n')
        .slice(0, 6)
        .join('\n');
      return {
        title: `Write ${basename(input.file_path)}?`,
        command: `${input.file_path}\n${body}`,
      };
    }
    const pretty = JSON.stringify(input, null, 2);
    return {
      title: `Allow ${tool}?`,
      command:
        pretty && pretty !== '{}'
          ? pretty.length > 400
            ? pretty.slice(0, 397) + '…'
            : pretty
          : null,
    };
  }
  if (q.kind === 'choice') {
    const qs = p.tool_input?.questions ?? [];
    return { title: (qs[0]?.question ?? '') || 'Choose an option', questions: qs };
  }
  if (q.kind === 'freeform') {
    return { title: (p.text ?? '') || '(question lost)' };
  }
  if (q.kind === 'elicitation') {
    return { title: (p.message ?? '') || 'Input requested', schema: p.requestedSchema ?? null };
  }
  return { title: '(unknown question)' };
}

// ---------------------------------------------------------------- image paste

// The first image item on a clipboard, or null. Pure selection logic so the
// terminal's paste handler stays a thin DOM shim: given event.clipboardData
// .items (or any array-like of {kind,type,getAsFile}), pick what the paste
// feature ingests. Text-only clipboards return null — the caller then chooses
// native xterm handling for one line or the daemon's atomic path for many.
// The shape the selector reads off each clipboard entry — deliberately a subset
// of DataTransferItem so a real `event.clipboardData.items` flows in unchanged,
// while the generic hands the concrete item (with .getAsFile) back to the caller.
interface ClipItem {
  kind?: string;
  type?: string;
}
export function imageFromClipboard<T extends ClipItem>(
  items: ArrayLike<T | null | undefined> | null | undefined,
): T | null {
  if (!items) return null;
  for (const it of Array.from(items)) {
    if (it && it.kind === 'file' && (it.type ?? '').startsWith('image/')) return it;
  }
  return null;
}

// ---------------------------------------------- tmux passthrough (OSC 52 only)

// Built from char codes, never written literally: an ESC in source is an
// invisible control character (the same reason NEWLINE_SEQ in TermPane.jsx is
// built this way).
const ESC = String.fromCharCode(27);
const PT_OPEN = ESC + 'Ptmux;';
const PT_CLOSE = ESC + '\\';
// A selection can be large, but not unbounded: past this a partial wrapper is
// flushed verbatim rather than buffered forever. Nothing is ever dropped for
// being long — it is only stopped from pinning memory on a stream that never
// closes the sequence.
const PT_MAX = 1 << 20;

/**
 * Unwrap tmux PASSTHROUGH sequences carrying OSC 52, and ONLY those.
 *
 * A program that wants to reach the real terminal through tmux wraps its escape
 * in `ESC P tmux ; <escape, every ESC doubled> ESC \`, and tmux unwraps it for
 * the client. Fleet Deck's client is tmux CONTROL MODE, which is not a
 * terminal — so tmux never unwraps anything, and the wrapper arrives at the
 * board intact. xterm then sees a DCS it does not know and discards the lot,
 * which is precisely how the agent's own "copied N characters" reached nobody.
 * The board is the terminal here, so the board does tmux's half of the job.
 *
 * ONLY OSC 52 is unwrapped. Passthrough is a licence to send the terminal
 * anything at all, and a pane renders bytes from files, tools and the network;
 * honouring the general case would hand every one of those a direct line to the
 * emulator. A clipboard WRITE is the one sequence worth carrying (and the
 * reader that would carry it back is refused in TermPane).
 *
 * Streaming-safe: returns {out, carry}. `carry` is a partial wrapper held back
 * for the next chunk — a socket frame can split a sequence anywhere.
 */
export function unwrapTmuxPassthrough(
  chunk: string | null | undefined,
  carry = '',
): { out: string; carry: string } {
  let buf = carry + (chunk ?? '');
  let out = '';
  for (;;) {
    const open = buf.indexOf(PT_OPEN);
    if (open === -1) break;
    out += buf.slice(0, open);
    const rest = buf.slice(open + PT_OPEN.length);
    const close = rest.indexOf(PT_CLOSE);
    if (close === -1) {
      // Incomplete — wait for more, unless it has grown past all reason.
      if (rest.length > PT_MAX) {
        out += PT_OPEN + rest;
        buf = '';
        break;
      }
      return { out, carry: PT_OPEN + rest };
    }
    const inner = rest
      .slice(0, close)
      .split(ESC + ESC)
      .join(ESC);
    // The whole point: keep the clipboard write, drop every other passthrough.
    if (inner.startsWith('\u001b]52;')) out += inner;
    buf = rest.slice(close + PT_CLOSE.length);
  }
  // Whatever is left holds no complete wrapper — but its TAIL may be the start
  // of one whose remainder arrives in the next frame. Hold that suffix back
  // rather than emitting it: emitted, the leading ESC of a split wrapper would
  // reach xterm, which keeps its own escape-parser state across writes and
  // would happily reassemble — and EXECUTE — a wrapper this filter exists to
  // drop. Whether a forbidden passthrough is filtered must never depend on
  // where a socket frame happened to end.
  for (let n = Math.min(PT_OPEN.length - 1, buf.length); n > 0; n--) {
    if (buf.endsWith(PT_OPEN.slice(0, n)))
      return { out: out + buf.slice(0, -n), carry: buf.slice(-n) };
  }
  return { out: out + buf, carry: '' };
}

// ------------------------------------------------------- copying out of a pane

// Is this a Mac? Only ever used to pick which chord to LISTEN for and which
// glyphs to PRINT, so a bad guess is cosmetic, never destructive. Reads the
// modern hint first and falls back to the deprecated (but universal) platform
// string; both are optional-chained so this file still loads under node --test.
// navigator is absent under `node --test`, and `userAgentData` is not in every
// lib.dom — this structural view keeps both `?.` guards honest and the modern
// field readable without a DOM-typed navigator.
interface NavLike {
  userAgentData?: { platform?: string };
  platform?: string;
}
export function isMacUA(): boolean {
  const nav = globalThis.navigator as NavLike | undefined;
  return /mac/i.test((nav?.userAgentData?.platform ?? '') || (nav?.platform ?? '') || '');
}

// The subset of a keydown/paste this file needs — a plain object literal in the
// tests satisfies it just as a real KeyboardEvent does, keeping these pure.
interface ChordEvent {
  type?: string;
  key?: string;
  ctrlKey?: boolean;
  metaKey?: boolean;
  shiftKey?: boolean;
  altKey?: boolean;
}

// Should this keydown COPY the pane's selection instead of reaching the agent?
//
// In a terminal Ctrl+C is not "copy" — it is ETX, the interrupt, and xterm sends
// it verbatim (that is why it is the one chord a terminal cannot simply hand to
// the browser). So the board does what every GUI terminal does: Ctrl+C copies
// ONLY when there is a selection to copy, and the caller clears that selection
// afterwards so the very next Ctrl+C interrupts the agent as it always has. With
// nothing selected this returns false and the keystroke is untouched.
//
// On a Mac the interrupt is Ctrl+C too, so we claim ⌘C there instead — a chord
// the TUI has no use for. Ctrl+Shift+C is deliberately NOT claimed: Chrome and
// Firefox both swallow it for devtools before a page ever sees it.
//
// Pure (a plain {type,key,ctrlKey,...} is enough) so it can be tested without a
// DOM — the DOM shim in TermPane stays a two-line call.
export function isTermCopyChord(
  e: ChordEvent | null | undefined,
  isMac: boolean = isMacUA(),
): boolean {
  if (e?.type !== 'keydown') return false;
  if ((e.key ?? '').toLowerCase() !== 'c') return false;
  if (e.shiftKey || e.altKey) return false;
  return isMac ? !!e.metaKey && !e.ctrlKey : !!e.ctrlKey && !e.metaKey;
}

// Should this keydown PASTE the browser's clipboard into the pane?
//
// In a terminal Ctrl+V is not paste — it is the raw byte ^V, and pasting is
// Ctrl+Shift+V. That convention exists because the terminal and the program
// share one machine. Here they do not: the clipboard is in a browser, possibly
// on another computer entirely, and Claude Code answers ^V by looking for an
// image on the DAEMON HOST's clipboard — a machine the human is not sitting at.
// It then says "no image found", truthfully and uselessly. So the board claims
// the chord and pastes what the human actually means: their own clipboard.
//
// The caller must NOT preventDefault: the whole trick is to stop xterm turning
// the chord into ^V while letting the BROWSER perform its own native paste. The
// resulting paste event is trusted and needs no clipboard-read permission.
// TermPane lets a single line reach xterm, while a multiline event takes the
// daemon's FIFO bracketed-paste path so its newlines never become live submits.
//
// On a Mac ⌘V is already the browser's paste and xterm never intercepts meta
// chords, so there is nothing to claim.
export function isTermPasteChord(
  e: ChordEvent | null | undefined,
  isMac: boolean = isMacUA(),
): boolean {
  if (isMac || e?.type !== 'keydown') return false;
  if ((e.key ?? '').toLowerCase() !== 'v') return false;
  return !!e.ctrlKey && !e.shiftKey && !e.altKey && !e.metaKey;
}

// The two things about a live pane that are NOT guessable, in the hint bar's
// voice. Selecting needs a modifier because the agent's TUI turns mouse
// reporting ON: without one, a drag is a drag the AGENT sees, and xterm makes no
// selection at all — which is exactly what "I copied and got nothing" feels
// like. xterm forces a local selection on Shift (⌥ on a Mac, which is why
// TermPane sets macOptionClickForcesSelection).
export function termChordHints(isMac: boolean = isMacUA()): { select: string; copy: string } {
  return isMac ? { select: '⌥drag', copy: '⌘C' } : { select: '⇧drag', copy: 'Ctrl+C' };
}
