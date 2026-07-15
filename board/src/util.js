// Small pure helpers shared across the board.

// Clipboard for the LAN panel. navigator.clipboard exists ONLY in a secure
// context — and the LAN board is plain http://192.168.x.x, which is exactly
// where copying a URL matters most. So: try the real API, fall back to the old
// execCommand trick, and tell the truth (false) when both are refused so the
// caller can say "select it yourself" instead of lying about a copy.
export async function copyText(text) {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch { /* denied or insecure context — fall through */ }
  try {
    const ta = document.createElement('textarea');
    ta.value = text;
    ta.setAttribute('readonly', '');
    ta.style.position = 'fixed';
    ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand('copy');
    document.body.removeChild(ta);
    return ok;
  } catch {
    return false;
  }
}

export function human(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 60) return s + 's';
  const m = Math.floor(s / 60);
  if (m < 60) return m + 'm';
  return Math.floor(m / 60) + 'h ' + (m % 60) + 'm';
}

export function hhmmss(t) {
  const d = new Date(t);
  const p = (n) => String(n).padStart(2, '0');
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

export function basename(p) {
  const s = String(p ?? '');
  const i = Math.max(s.lastIndexOf('/'), s.lastIndexOf('\\'));
  return i === -1 ? s : s.slice(i + 1);
}

// M-S1 — `remote.url` is harvested by the daemon from the AGENT's terminal
// output, which is the least-trusted party in the whole system. It reaches the
// board and lands in window.open()/<a href>, so a `javascript:` (or `data:`)
// URL there would EXECUTE on click. Nothing legitimate lives outside claude.ai,
// so gate every use of that URL through here: it returns the URL only when it
// parses to https on claude.ai (or a subdomain), and null for everything else —
// the caller then renders the plain, non-clickable chip instead of a live link.
export function safeUrl(u) {
  try {
    const url = new URL(String(u ?? '').trim());
    if (url.protocol === 'https:'
      && (url.hostname === 'claude.ai' || url.hostname.endsWith('.claude.ai'))) {
      return url.href;
    }
  } catch { /* not a parseable URL */ }
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
function parseModel(model) {
  let s = String(model ?? '').trim();
  if (!s) return null;

  // The long-context marker must come off BEFORE tokenizing: the build-tag
  // filter below matches '1m' and would silently eat it. It arrives bracketed
  // from the CLI ('...[1m]') and bare from our own output ('Opus 4.8 1M') —
  // accept both, so prettyModel is idempotent over what it just rendered.
  let marker = null;
  const bracketed = s.match(/\[([^\]]+)\]\s*$/);
  const bare = s.match(/[\s\-_](\d+m)\s*$/i);
  if (bracketed) { marker = bracketed[1].toUpperCase(); s = s.slice(0, bracketed.index); }
  else if (bare) { marker = bare[1].toUpperCase(); s = s.slice(0, bare.index); }

  const toks = s
    .replace(/^claude[-_ ]/i, '')
    .split(/[-_ .]+/) // '.' too, so prettyModel is idempotent over its own output
    .filter(Boolean)
    .filter((t) => !/^\d{6,}$/.test(t))        // 20250929 datestamps
    .filter((t) => !/^v?\d+[a-z]\d*$/i.test(t)); // build tags

  const parts = [];
  for (let i = 0; i < toks.length; i++) {
    if (/^\d+$/.test(toks[i])) {
      const run = [];
      while (i < toks.length && /^\d+$/.test(toks[i])) run.push(toks[i++]);
      i--;
      parts.push({ ver: true, text: run.join('.') });
    } else {
      parts.push({ ver: false, text: toks[i][0].toUpperCase() + toks[i].slice(1).toLowerCase() });
    }
  }
  // Legacy ids put the version first ('claude-3-5-haiku'). Hoist it behind the
  // name so the badge never shows '3.5 Haiku' beside a 'Haiku 4.5'.
  if (parts.length > 1 && parts[0].ver) parts.push(parts.shift());

  return { parts, marker, raw: String(model).trim() };
}

// 'claude-opus-4-8[1m]' → 'Opus 4.8 1M' · 'claude-3-5-haiku-20241022' → 'Haiku 3.5'
export function prettyModel(model) {
  const p = parseModel(model);
  if (!p) return '—';
  const name = p.parts.map((x) => x.text).join(' ');
  if (!name) return p.raw;
  return p.marker ? `${name} ${p.marker}` : name;
}

// Compact cards: initials + version, no marker — 'claude-opus-4-8[1m]' → 'O4.8'.
export function modelShort(model) {
  const p = parseModel(model);
  if (!p) return '—';
  return p.parts.map((x) => (x.ver ? x.text : x.text[0])).join('') || p.raw;
}

// model family → CSS class carrying the --m-* token pair
export function modelFamily(model) {
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

export function parseBatchTasks(text) {
  return String(text ?? '')
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const m = line.match(BATCH_REPEAT_RE);
      if (!m) return { count: 1, prompt: line };
      return { count: Math.max(1, parseInt(m[1], 10)), prompt: m[2].trim() };
    })
    .filter((t) => t.prompt);
}

/** How many agents `parseBatchTasks` output would actually launch. */
export function batchTotal(tasks) {
  return (tasks || []).reduce((n, t) => n + t.count, 0);
}

/** The flat prompt list, in launch order — one entry per agent. */
export function expandBatchTasks(tasks) {
  return (tasks || []).flatMap((t) => Array.from({ length: t.count }, () => t.prompt));
}

// v1.4 live terminal — a board-owned pane is viewable while the pane exists:
// spawning (incl. the stalled watchdog state) or live; never after exit/kill.
export function spawnTermable(s) {
  if (!s?.spawn) return false;
  const st = s.spawn.stalled ? 'stalled' : (s.spawn.status || 'live');
  return st === 'spawning' || st === 'stalled' || st === 'live';
}

// v1.8 kill-from-the-card — a board-owned pane can be killed while a window
// still exists to kill: spawning / stalled / live, and pane-dead (the dead
// pane's window survives until something kills it). 'killed' and 'gone' are
// terminal — there is nothing left to take down, so the board offers nothing.
export function spawnKillable(s) {
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
export function spawnRemoteAvailable(s) {
  if (!s?.spawn || s.spawn.status !== 'live' || s.spawn.remote?.enabled) return false;
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
export function adoptableNow(s) {
  return s?.adopt?.eligible === 'now';
}
export function adoptArmable(s) {
  return s?.adopt?.eligible === 'arm';
}
export function adoptArmed(s) {
  const a = s?.adopt;
  return !!(a && a.armed && a.armed_until && a.armed_until > Date.now());
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

export function boardCol(col) {
  if (col === 'needsyou') return 'working';
  return COLS.some((c) => c.key === col) ? col : 'idle';
}

// The pulse-dot class for a session's column — the amber "working" glow, the
// verifying tint, the needsyou amber, the offline grey, or the resting "still".
// SessionCard's dot and the Drawer head both render this off `s.col`; one chain
// here so the two surfaces can never drift apart (D12).
export function colPulse(col) {
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
export function worktreeLabel(s) {
  return s.worktree && basename(s.worktree) !== s.repo_name ? basename(s.worktree) : null;
}

// The session lookup every surface rebuilds: session_id → session. Built fresh
// from the current snapshot — sessions are replaced wholesale each frame, so
// there is nothing to memo across frames beyond what the caller already does
// (D12: App, BoardLanes and Inbox each open-coded this Map).
export function sessionsById(sessions) {
  return new Map(sessions.map((s) => [s.session_id, s]));
}

// Prefer a session's callsign, falling back to the raw id when the session has
// been archived out of the snapshot — used wherever a conflict/mail names a sid
// the board may no longer be showing.
export function callsignOf(byId, sid) {
  return byId.get(sid)?.callsign || sid;
}

// ------------------------------------------------------------ callsign naming
// A callsign is `<animal>-<suffix>`: 'wren-a9e1' (suffix = the first 4 chars of
// the session uuid), 'wren-PROJ-123' (a ticket), or 'wren-docs-review' (a human
// name). The ANIMAL is minted by the daemon and is never chosen — only the
// SUFFIX is renameable — so the split is always at the FIRST dash: everything
// after it, dashes included, is ONE suffix ('PROJ-123' is not two fields).
export function animalOf(callsign) {
  const s = String(callsign ?? '');
  const i = s.indexOf('-');
  return i === -1 ? s : s.slice(0, i);
}
export function suffixOf(callsign) {
  const s = String(callsign ?? '');
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
export function validSuffix(s) {
  return SUFFIX_RE.test(String(s ?? ''));
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
export function sessionTicker(ticker, callsign) {
  const esc = String(callsign ?? '').replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const re = new RegExp(`(^|[^A-Za-z0-9-])${esc}([^A-Za-z0-9-]|$)`);
  return (ticker || [])
    .filter((e) => re.test(String(e.msg || '')))
    .slice(0, 12);
}

// The one sentence the board repeats wherever a message won't be delivered
// instantly: it rides the agent's turn loop, so it lands at the next boundary.
// Quoted verbatim across Compose/Inbox/Drawer/PlanLibrary — one constant so the
// copy stays identical and changes in a single place (D12).
export const TURN_BOUNDARY_HINT = 'next turn boundary — idle sessions usually wake within seconds';

// Ticker rows are {at, msg} only — the tag is derived from the daemon's
// message conventions (derive.mjs / questions.mjs tick() calls).
export function classifyTicker(msg) {
  const m = String(msg ?? '');
  if (m.startsWith('⚠')) return 'confl';
  if (/^(🖐|❓|⌛|✅|💬)/.test(m)) return 'ask';
  if (/^(✉|📣|📌|📝)/.test(m)) return 'mail';
  if (/joined the fleet|left the fleet/.test(m)) return 'join';
  return 'tool';
}

export function stripTickerEmoji(msg) {
  return String(msg ?? '').replace(/^(⚠|🖐|❓|⌛|✅|💬|✉|📣|📌|📝)\s*/u, '');
}

// Question title + command block per kind, from the raw hook payload.
export function questionView(q) {
  const p = q.payload || {};
  if (q.kind === 'permission') {
    const tool = p.tool_name || 'tool';
    const input = p.tool_input || {};
    if (tool === 'Bash' && input.command) {
      const cmd = String(input.command);
      return {
        title: `Run \`${cmd.length > 60 ? cmd.slice(0, 57) + '…' : cmd}\`?`,
        command: '$ ' + cmd,
      };
    }
    if ((tool === 'Edit' || tool === 'MultiEdit') && input.file_path) {
      const olds = String(input.old_string ?? '');
      const news = String(input.new_string ?? '');
      const lines = [
        ...olds.split('\n').filter(Boolean).slice(0, 6).map((l) => ({ kind: 'del', text: '- ' + l })),
        ...news.split('\n').filter(Boolean).slice(0, 6).map((l) => ({ kind: 'add', text: '+ ' + l })),
      ];
      return { title: `Edit ${basename(input.file_path)}?`, command: input.file_path, diff: lines };
    }
    if (tool === 'Write' && input.file_path) {
      const body = String(input.content ?? '').split('\n').slice(0, 6).join('\n');
      return { title: `Write ${basename(input.file_path)}?`, command: `${input.file_path}\n${body}` };
    }
    const pretty = JSON.stringify(input, null, 2);
    return {
      title: `Allow ${tool}?`,
      command: pretty && pretty !== '{}' ? (pretty.length > 400 ? pretty.slice(0, 397) + '…' : pretty) : null,
    };
  }
  if (q.kind === 'choice') {
    const qs = p.tool_input?.questions || [];
    return { title: qs[0]?.question || 'Choose an option', questions: qs };
  }
  if (q.kind === 'freeform') {
    return { title: p.text || '(question lost)' };
  }
  if (q.kind === 'elicitation') {
    return { title: p.message || 'Input requested', schema: p.requestedSchema || null };
  }
  return { title: '(unknown question)' };
}

// ---------------------------------------------------------------- image paste

// The first image item on a clipboard, or null. Pure selection logic so the
// terminal's paste handler stays a thin DOM shim: given event.clipboardData
// .items (or any array-like of {kind,type,getAsFile}), pick what the paste
// feature ingests. Text-only clipboards return null — the caller must then let
// the event fall through to xterm untouched, so ordinary text paste keeps
// working exactly as before.
export function imageFromClipboard(items) {
  if (!items) return null;
  for (const it of Array.from(items)) {
    if (it && it.kind === 'file' && /^image\//.test(it.type || '')) return it;
  }
  return null;
}
