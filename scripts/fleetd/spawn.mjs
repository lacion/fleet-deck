// spawn.mjs — the v1.2 tmux adapter (v1.2 — dynamic fleet). One backend,
// tmux, verbatim from the firstmate analysis: create/exists/alive/kill
// primitives, nothing more.
//
// Non-negotiables enforced here:
//   - ALL tmux command construction is argv arrays through execFile — never a
//     shell string containing user text. Verified on tmux 3.7b: multi-arg
//     new-window execvp()s the command verbatim (argc preserved, `;`/`$()`/
//     quotes arrive as literal bytes in the child's argv — no shell exists to
//     interpret them).
//   - Scoped names everywhere (firstmate's cross-home collision lesson):
//     session `fleetdeck-<port>`, windows `fd<port>-<callsign>`. Every
//     list/kill path matches the exact scoped name, never a bare index.
//   - Windows get `remain-on-exit on`: claude's exit (or SIGKILL) must not
//     vaporize the pane — the human may want the scrollback (CONTRACT), and a
//     dead pane is the deterministic crash signal for owned-pane liveness.
//     Verified: a dead pane keeps reporting the ORIGINAL command in
//     #{pane_current_command} (with #{pane_dead}=1), so liveness checks MUST
//     read pane_dead too — the command name alone would say "claude" forever.
//   - FLEETDECK_TMUX_SOCKET selects an isolated tmux server with `-L <name>`
//     for every adapter command. Blank values retain tmux's default socket.
//
// Test override (CONTRACT): FLEETDECK_SPAWN_CMD — when set, the daemon runs
// argv [FLEETDECK_SPAWN_CMD, JSON.stringify(spec)] instead of tmux; the
// fixture records the spec and may itself POST /hook/SessionStart with the
// pre-issued session_id. Capability reports available:true, reason
// 'test-override'.

import { execFile, execFileSync, spawn as spawnChild } from 'node:child_process';
import { randomUUID } from 'node:crypto';

const TMUX_TIMEOUT_MS = 5_000;
const US = '\u001f'; // unit separator — never appears in tmux names in practice

// Run one tmux command (argv). Resolves stdout on success, null on ANY
// failure (tmux absent, no server, bad target, timeout) — callers decide
// whether null is "gone", "unknown" or an error.
function tmux(args) {
  return new Promise((resolve) => {
    try {
      const socket = process.env.FLEETDECK_TMUX_SOCKET?.trim();
      const argv = socket ? ['-L', socket, ...args] : args;
      execFile('tmux', argv, { timeout: TMUX_TIMEOUT_MS, windowsHide: true }, (err, stdout) => {
        resolve(err ? null : (stdout ?? ''));
      });
    } catch {
      resolve(null);
    }
  });
}

// ------------------------------------------------------------- capability
let probe = { ok: false, at: 0 };
const PROBE_TTL_MS = 60_000;

/** tmux binary reachable? Cached (60 s TTL) — this runs inside /health and
 * /state snapshots, so it must not fork a subprocess on every heartbeat. */
export function hasTmux() {
  const now = Date.now();
  if (now - probe.at < PROBE_TTL_MS) return probe.ok;
  let ok = false;
  try {
    execFileSync('tmux', ['-V'], { timeout: 1_500, stdio: ['ignore', 'pipe', 'ignore'] });
    ok = true;
  } catch { /* not installed / not executable */ }
  probe = { ok, at: now };
  return ok;
}

/** FLEETDECK_SPAWN_CMD override, or null when unset/blank. */
export function spawnOverrideCmd() {
  const v = process.env.FLEETDECK_SPAWN_CMD;
  return v && v.trim() ? v : null;
}

// ------------------------------------------------------------ scoped names
export const sessionName = port => `fleetdeck-${port}`;
export const windowName = (port, callsign) => `fd${port}-${callsign}`;

// ----------------------------------------------------------------- session
/** Ensure the detached daemon-owned session `fleetdeck-<port>` exists.
 * `=` prefix = exact session-name match (verified; prefix matching could
 * otherwise confuse fleetdeck-4711 with fleetdeck-47110). */
export async function ensureSession(port) {
  const name = sessionName(port);
  if (await tmux(['has-session', '-t', '=' + name]) !== null) return name;
  if (await tmux(['new-session', '-d', '-s', name]) !== null) return name;
  // lost a creation race? re-check before failing loud
  if (await tmux(['has-session', '-t', '=' + name]) !== null) return name;
  throw new Error(`tmux could not create session ${name}`);
}

// ----------------------------------------------------------------- windows
/** Create a detached window named fd<port>-<callsign> in fleetdeck-<port>,
 * cwd set, running `argv` DIRECTLY (execvp, no shell — see header). Returns
 * {session, window, window_id} — window_id (@n) is the stable kill/inspect
 * target, immune to renames and index shuffles. */
export async function newWindow({ port, callsign, cwd, argv }) {
  const session = sessionName(port);
  const window = windowName(port, callsign);
  const out = await tmux([
    'new-window', '-d', '-P', '-F', '#{window_id}',
    '-t', '=' + session + ':', // exact session, next free window index
    '-n', window,
    '-c', cwd,
    '--', ...argv,
  ]);
  if (out === null) throw new Error(`tmux new-window failed for ${window}`);
  const window_id = out.trim();
  // Best-effort: keep the pane (scrollback + deterministic pane_dead crash
  // signal) when the command exits. A failure here degrades gracefully — the
  // window just closes on exit and boot reconciliation marks the row 'gone'.
  await tmux(['set-option', '-w', '-t', window_id, 'remain-on-exit', 'on']);
  return { session, window, window_id };
}

/** {dead, cmd} for a pane/window target (@id or scoped name), or null when
 * the target is gone / tmux unreachable (= UNKNOWN, never confidently dead). */
export async function paneCurrentCommand(target) {
  const out = await tmux(['display-message', '-p', '-t', target, `#{pane_dead}${US}#{pane_current_command}`]);
  if (out === null) return null;
  const [dead, cmd] = out.replace(/\n$/, '').split(US);
  return { dead: dead === '1', cmd: cmd ?? '' };
}

/** All windows on the server whose name matches this fleet's scope
 * (`fd<port>-*`), with the first (lowest-index) pane speaking for each
 * window: [{session, window, window_id, pane_dead, pane_cmd}]. Returns []
 * when tmux is unreachable / no server runs. */
export async function listScopedWindows(port) {
  const out = await tmux([
    'list-panes', '-a', '-F',
    ['#{session_name}', '#{window_name}', '#{window_id}', '#{pane_dead}', '#{pane_current_command}'].join(US),
  ]);
  if (out === null) return [];
  const prefix = `fd${port}-`;
  const seen = new Set();
  const wins = [];
  for (const line of out.split('\n')) {
    if (!line) continue;
    const [session, window, window_id, dead, cmd] = line.split(US);
    if (!window || !window.startsWith(prefix)) continue;
    if (seen.has(window_id)) continue; // human split the pane: original pane wins
    seen.add(window_id);
    wins.push({ session, window, window_id, pane_dead: dead === '1', pane_cmd: cmd ?? '' });
  }
  return wins;
}

/** Name-verified kill (CONTRACT): re-locate the window by its EXACT scoped
 * name at kill time and kill by window_id — a renamed/recycled window can
 * never be mis-killed via a stale index. Returns:
 *   {ok:true, window_id}   killed
 *   {ok:false, gone:true}  no window with that exact name exists (410)
 *   {ok:false, error}      tmux kill-window itself failed */
export async function killWindowVerified(name) {
  if (!name) return { ok: false, gone: true };
  const out = await tmux(['list-panes', '-a', '-F', `#{window_name}${US}#{window_id}`]);
  if (out === null) return { ok: false, gone: true }; // no server ⇒ no window
  const hit = out.split('\n').filter(Boolean).map(l => l.split(US)).find(([w]) => w === name);
  if (!hit) return { ok: false, gone: true };
  if (await tmux(['kill-window', '-t', hit[1]]) !== null) return { ok: true, window_id: hit[1] };
  // kill failed — vanished between list and kill, or a real tmux error?
  const again = await tmux(['list-panes', '-a', '-F', '#{window_name}']);
  if (again === null || !again.split('\n').includes(name)) return { ok: false, gone: true };
  return { ok: false, error: 'tmux kill-window failed' };
}

/** Neutralize the BRACKETED-PASTE BREAKOUT before any owned-pane paste
 * (CONTRACT). pasteText delivers with `-p`, which wraps the buffer in tmux's
 * bracketed-paste markers ESC[200~ … ESC[201~. Mail delivery (mail.mjs) pipes
 * VERBATIM message content through pasteText, so content carrying a literal END
 * marker `\x1b[201~` would close the bracket EARLY inside the receiving Claude
 * TUI — everything after it is then processed as LIVE keystrokes, which the
 * daemon's own sendEnter promptly submits. That is keystroke/command injection
 * into a daemon-owned pane. This is the one chokepoint every pane paste flows
 * through, so sanitizing here protects every caller.
 *
 * Pure and conservative: normalize CRLF / lone CR to LF, delete the
 * bracketed-paste START/END markers (looped so a crafted overlap cannot
 * reconstitute one after a single pass), then strip every remaining C0 control
 * byte — bare ESC `\x1b` included, since it could open a fresh control sequence —
 * plus DEL and the C1 controls (0x80–0x9f, e.g. the 8-bit CSI U+009B) — EXCEPT
 * `\t` (0x09) and `\n` (0x0A), both legitimate in pasted text. Code points above
 * U+009F are never touched, so normal UTF-8 (accented Latin-1 at U+00A0–U+00FF,
 * CJK, emoji) is intact. The control strip is the load-bearing guarantee: with no
 * ESC or C1 CSI left, no functional paste marker can survive whatever the input
 * tried to smuggle in. */
export function sanitizePaneText(text) {
  let out = String(text).replace(/\r\n?/g, '\n');
  let prev;
  do { prev = out; out = out.replace(/\u001b\[20[01]~/g, ''); } while (out !== prev);
  // Strip C0 controls (keep \t and \n), DEL, AND the C1 range 0x80-0x9f. The
  // C1 strip closes the 8-bit-CSI form: a lone U+009B is the single-byte CSI a
  // terminal could read as the start of a `\u001b[201~` bracketed-paste terminator,
  // so removing it denies that alternative escape. C1 code points are control
  // codes, never legitimate text; everything above U+009F (accented Latin-1,
  // CJK, emoji) is untouched.
  return out.replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, '');
}

/** The four sanctioned owned-pane injections (CONTRACT) are: one bring-up
 * Enter for the trust dialog, bracketed-paste mail followed by Enter, and
 * verbatim human typing relayed by the live-terminal modal, plus a human's
 * explicit board action enabling remote control via a literally typed /rc
 * command. All user text still travels without a shell; terminal input uses
 * control-mode hex bytes. */
export async function pasteText(target, text) {
  // Bracketed-paste breakout defense: sanitize BEFORE the buffer is set, so the
  // `-p` paste below can never carry an END marker that turns mail content into
  // live keystrokes (see sanitizePaneText).
  const safe = sanitizePaneText(text);
  // tmux buffers are server-global, so a constant name lets concurrent mail
  // deliveries overwrite each other between set-buffer and paste-buffer. A
  // UUID makes the two-command handoff private to this call; `-d` removes the
  // buffer on success, while finally covers a failed/timed-out paste.
  const buffer = `fdmail-${randomUUID()}`;
  if (await tmux(['set-buffer', '-b', buffer, '--', safe]) === null) return false;
  try {
    return (await tmux(['paste-buffer', '-p', '-d', '-b', buffer, '-t', target])) !== null;
  } finally {
    // Best-effort and deliberately awaited: do not leave mail text resident
    // in tmux when paste-buffer fails before its `-d` cleanup can take effect.
    await tmux(['delete-buffer', '-b', buffer]);
  }
}

export async function sendEnter(target) {
  return (await tmux(['send-keys', '-t', target, 'Enter'])) !== null;
}

/** Literal keystrokes for TUI commands. `-l --` prevents tmux key-name
 * parsing; unlike bracketed paste this reaches Claude as typed slash input. */
export async function typeKeys(target, text) {
  return (await tmux(['send-keys', '-t', target, '-l', '--', String(text)])) !== null;
}

/** Independent pane-scrollback capture for remote-control URL harvesting.
 * Keep this adapter local rather than coupling daemon state to termbridge. */
export async function capturePane(target) {
  return tmux(['capture-pane', '-p', '-t', target]);
}

/** Bring-up compatibility export; caller enforces at-most-once per spawn. */
export async function sendBringupEnter(target) {
  return sendEnter(target);
}

// ------------------------------------------------------------ test override
/** Launch the FLEETDECK_SPAWN_CMD fixture: argv [cmd, JSON.stringify(spec)],
 * detached, output ignored. onError fires if the process can't start at all
 * (bad path) — asynchronous by nature, so the spawn row simply stays
 * 'spawning' and the caller's note explains why. */
export function launchOverride(cmd, spec, onError = () => {}) {
  try {
    const child = spawnChild(cmd, [JSON.stringify(spec)], { stdio: 'ignore', detached: true });
    child.on('error', err => { try { onError(err); } catch { /* reporting only */ } });
    child.unref();
  } catch (err) {
    try { onError(err); } catch { /* reporting only */ }
  }
}
