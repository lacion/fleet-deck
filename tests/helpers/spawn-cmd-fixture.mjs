#!/usr/bin/env node
// tests/helpers/spawn-cmd-fixture.mjs
//
// Stand-in for the real tmux spawn backend, used as the FLEETDECK_SPAWN_CMD
// test override (v1.2 — dynamic fleet, "Test override" behavior):
//
//   "Test override: FLEETDECK_SPAWN_CMD — when set, instead of tmux the
//    daemon spawns argv [FLEETDECK_SPAWN_CMD, JSON.stringify(spec)] (fixture
//    records the spec and may itself POST /hook/SessionStart); capability
//    reports available:true with reason 'test-override'."
//
// The contract's own wording — "argv [FLEETDECK_SPAWN_CMD, JSON.stringify(spec)]"
// — describes a literal argv array, i.e. the daemon is expected to exec this
// file DIRECTLY (spawn(cmd, [json]), no shell), unlike the FLEETDECK_AGENTS_CMD
// convention (tests/helpers/agents-cmd-fixture.mjs) which is a shell command
// STRING run via exec(). That means this file must carry a shebang and be
// chmod +x (done once, at repo-write time — see tests/spawn.test.mjs, which
// verifies executability defensively rather than assuming it stays set), and
// FLEETDECK_SPAWN_CMD must be set to its bare path — never "node <path>".
//
// Responsibilities:
//  1. Record every invocation. argv[2] is the raw JSON.stringify(spec) string
//     the daemon handed us; append it (plus a best-effort parse) as one JSONL
//     line to FLEETDECK_TEST_SPAWN_RECORD. Tests read this file back to prove
//     argv construction: the string must survive intact (single element, no
//     shell re-tokenization / metachar interpretation) even when it embeds
//     shell-hostile characters like `"; rm -rf`.
//  2. If FLEETDECK_TEST_SPAWN_POST_URL is set, best-effort locate a session
//     id and cwd inside the parsed spec (field names are not pinned by the
//     contract text beyond the spawns-row column list, so this searches
//     recursively rather than assuming one exact shape) and POST
//     /hook/SessionStart for it, mimicking the real pane's first hook event
//     so spawn-happy-path tests can observe the source 'spawned' -> 'hooks'
//     flip and the spawns row going 'live' without a second real process.
//  3. Never throws past main() and always exits 0 — a fixture crash must
//     never look like "the spawn backend itself failed" to the daemon under
//     test (that failure mode is for the daemon's OWN process-spawn error
//     handling to cover, not this stand-in).

import { appendFileSync } from 'node:fs';

const raw = process.argv[2] ?? '';

function findByKeys(value, keys, seen = new Set()) {
  if (value == null || typeof value !== 'object') return null;
  if (seen.has(value)) return null;
  seen.add(value);
  for (const k of keys) {
    if (typeof value[k] === 'string' && value[k]) return value[k];
  }
  for (const v of Object.values(value)) {
    if (v && typeof v === 'object') {
      const found = findByKeys(v, keys, seen);
      if (found) return found;
    }
  }
  return null;
}

function findSessionId(spec) {
  return findByKeys(spec, ['session_id', 'sessionId', 'uuid', 'session']);
}

function findCwd(spec) {
  return findByKeys(spec, ['cwd', 'worktree_path', 'worktreePath']);
}

async function main() {
  let parsed = null;
  let parseError = null;
  try {
    parsed = JSON.parse(raw);
  } catch (err) {
    parseError = String(err?.message || err);
  }

  const recordFile = process.env.FLEETDECK_TEST_SPAWN_RECORD;
  if (recordFile) {
    try {
      appendFileSync(recordFile, JSON.stringify({ raw, parsed, parseError, receivedAt: Date.now() }) + '\n');
    } catch { /* best-effort recording only */ }
  }

  const postUrl = process.env.FLEETDECK_TEST_SPAWN_POST_URL;
  if (postUrl && parsed) {
    const sid = findSessionId(parsed);
    const cwd = findCwd(parsed) || process.cwd();
    if (sid) {
      try {
        await fetch(`${postUrl}/hook/SessionStart`, {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({
            session_id: sid,
            hook_event_name: 'SessionStart',
            cwd,
            source: 'startup',
          }),
          signal: AbortSignal.timeout(5000),
        });
      } catch { /* best-effort -- the waiting test will time out and report it */ }
    }
  }
}

main().finally(() => process.exit(0));
