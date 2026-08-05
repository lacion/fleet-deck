// tests/demo-accept-isolation.test.mjs
//
// BUG-097 regression: overlapping demo acceptance runs must not destroy or
// control each other's scratch fleet. The v1.2 (spawn), v1.3 (plan), and
// Phase 3 gates used to share port 4711, the .fleetdeck-test home, the
// demo/project fixture, and demo/demo-logs — each reset read the same
// pidfile, removed the same home, and competed for the same port, so one
// run could kill the other's daemon and delete active state. The gates now
// mktemp a unique daemon home + fixture copy and bind a kernel-assigned
// free port per run; this file statically pins those properties, then
// functionally proves two concurrent plan-gate daemons on unique resources
// survive each other's exit (no Claude CLI is launched, so nothing is
// billed).

import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(HERE, '..');
const DEMO = path.join(REPO_ROOT, 'demo');
const FLEETD = path.join(REPO_ROOT, 'scripts/fleetd/fleetd.mjs');

const bash = (script) =>
  execFileSync('bash', ['-c', script], { encoding: 'utf8', cwd: DEMO }).trim();

// The plan gate's provenance guard, extracted and run exactly as the script
// runs it. A run may never kill, fuser, reset, or share state with anything
// it did not create itself.
const PROVENANCE_GUARD = String.raw`
  for bad in \
    'REAL_HOME=' \
    '.fleetdeck-test' \
    'fuser' \
    '\$SCRIPT_DIR/project' \
    'FLEETDECK_PORT=4711' \
    'DEMO_LOGS="\$SCRIPT_DIR/' ; do
    if grep -qF -- "$bad" run-accept-plan.sh; then
      echo "shared/global resource reference remains: $bad" >&2
      exit 1
    fi
  done
  grep -qF 'SCRATCH_HOME="$(mktemp -d ' run-accept-plan.sh || exit 1
  grep -qF 'PROJECT_DIR="$(mktemp -d ' run-accept-plan.sh || exit 1
  grep -qF 'cp -R "$SEED_PROJECT/." "$PROJECT_DIR/"' run-accept-plan.sh || exit 1
  grep -qF 'probe.listen(0, "127.0.0.1"' run-accept-plan.sh || exit 1
`;

// The plan gate's resource setup: mktemp home + fixture copy, verified-free
// kernel-assigned port, per-run derived names. Sourcing these lines against
// a redirected SEED_PROJECT exercises the same code the gate runs.
const SETUP_SNIPPET = String.raw`
  awk '
    /^SCRIPT_DIR=/ { print "SEED_PROJECT=\"$CLONE_PROJ\""; next }
    /^SCRATCH_HOME="\$\(mktemp -d /,/^rm -f "\$TEST_FILE"$/ { print }
  ' run-accept-plan.sh > "$CLONE_SETUP"
`;

test('BUG-097: demo acceptance gates allocate unique per-run resources', () => {
  const spawnGate = readFileSync(path.join(DEMO, 'run-accept-spawn.sh'), 'utf8');
  const planGate = readFileSync(path.join(DEMO, 'run-accept-plan.sh'), 'utf8');
  const phase3Gate = readFileSync(path.join(DEMO, 'run-accept-phase3.sh'), 'utf8');

  for (const [name, text] of [['run-accept-spawn.sh', spawnGate], ['run-accept-plan.sh', planGate]]) {
    assert.ok(
      !text.includes('.fleetdeck-test'),
      `${name} still references the shared .fleetdeck-test home — concurrent runs reset each other`,
    );
    assert.ok(
      !text.includes('REAL_HOME=') && !text.includes('fuser'),
      `${name} still kills the real daemon pidfile or force-clears a fixed port`,
    );
    assert.ok(
      !text.includes('FLEETDECK_PORT=4711') && !text.includes('PROJECT_DIR="$SCRIPT_DIR/project"'),
      `${name} still hardcodes port 4711 or uses the shared demo/project fixture directly`,
    );
    assert.match(text, /SCRATCH_HOME="\$\(mktemp -d /, `${name} must mktemp a unique daemon home`);
    assert.match(text, /PROJECT_DIR="\$\(mktemp -d /, `${name} must mktemp a unique fixture copy`);
    assert.ok(
      text.includes('cp -R "$SEED_PROJECT/." "$PROJECT_DIR/"'),
      `${name} must seed the fixture copy from the read-only demo/project`,
    );
    assert.ok(
      text.includes('probe.listen(0, "127.0.0.1"'),
      `${name} must bind a kernel-assigned verified-free port`,
    );
  }

  // Phase 3 keeps its historical overrideable defaults (4711 + .fleetdeck-test)
  // but must reset them only when they are the script's own defaults — never
  // unconditionally kill the production pidfile or fuser the port.
  assert.ok(!phase3Gate.includes('REAL_HOME='), 'run-accept-phase3.sh still kills the real daemon pidfile');
  assert.ok(!phase3Gate.includes('fuser'), 'run-accept-phase3.sh still force-clears a port by name');
  const resetIdx = phase3Gate.indexOf('rm -rf "$SCRATCH_HOME"');
  const resetCond = phase3Gate.lastIndexOf('SCRATCH_DEFAULTED=1', resetIdx);
  assert.ok(
    resetIdx > 0 && resetCond > 0 && resetIdx - resetCond < 500,
    'run-accept-phase3.sh must rm -rf the scratch home only when SCRATCH_DEFAULTED=1',
  );
  assert.match(phase3Gate, /WORK_ROOT="\$\(mktemp -d /, 'run-accept-phase3.sh must mktemp a unique fixture/evidence root');
  assert.ok(
    phase3Gate.includes('cp -R "$SEED_PROJECT/." "$PROJECT_DIR/"'),
    'run-accept-phase3.sh must copy the fixture instead of editing demo/project',
  );
});

test('BUG-097: two concurrent plan-gate daemons never touch each other', { timeout: 60000 }, async (t) => {
  if (process.platform !== 'linux') {
    return t.skip('the plan gate only spawns its daemon on tmux-capable Linux hosts');
  }
  let tmuxOk = true;
  try {
    execFileSync('tmux', ['-V'], { stdio: 'ignore' });
  } catch {
    tmuxOk = false;
  }
  if (!tmuxOk) return t.skip('no tmux on PATH — the plan gate would stop before spawning a daemon');

  // Static guard first: the shipped gate must still carry the unique-resource
  // discipline this functional run depends on.
  execFileSync('bash', ['-c', PROVENANCE_GUARD], { cwd: DEMO, stdio: 'pipe' });

  const runs = [];
  const cleanup = () => {
    for (const run of runs.splice(0)) {
      try { run.daemon.kill('SIGTERM'); } catch { /* already gone */ }
      try { execFileSync('tmux', ['-L', run.socket, 'kill-server'], { stdio: 'ignore' }); } catch { /* no server */ }
      for (const dir of [run.scratch, run.project, run.cloneProj, run.root]) {
        if (dir) rmSync(dir, { recursive: true, force: true });
      }
    }
  };

  // Sourced setup lines need CLONE_PROJ + CLONE_SETUP in the bash env.
  const exportLine = (k, v) => `export ${k}=${JSON.stringify(v)}`;

  const startRun = async (label) => {
    const root = mkdtempSync(path.join(tmpdir(), `fleetdeck-bug097-${label}-`));
    const cloneProj = path.join(root, 'seed-project');
    const setupFile = path.join(root, 'setup.sh');
    execFileSync('cp', ['-R', path.join(DEMO, 'project'), cloneProj]);
    const snippet = bash(`${exportLine('CLONE_PROJ', cloneProj)}; ${exportLine('CLONE_SETUP', setupFile)}; ${SETUP_SNIPPET}`);
    assert.ok(snippet === '', `setup extraction printed unexpectedly: ${snippet}`);
    const setup = readFileSync(setupFile, 'utf8');
    assert.match(setup, /mktemp -d/, 'extracted setup must contain the mktemp allocations');

    const socket = `fdbug097-${label}-${process.pid}`;
    const marker = path.join(root, 'run-paths.txt');
    const env = {
      PATH: process.env.PATH,
      HOME: process.env.HOME,
      TMPDIR: process.env.TMPDIR,
      FLEETDECK_TMUX_SOCKET: socket,
      TERM: process.env.TERM || 'xterm',
    };
    // Test-side bookkeeping only: after sourcing the gate's own setup lines,
    // record where they landed so the test can find this run's mktemp paths.
    const shell = spawn('bash', ['-c',
      `${exportLine('CLONE_PROJ', cloneProj)}; source "$CLONE_SETUP"; ` +
      `printf '%s\\n' "$SCRATCH_HOME" "$PROJECT_DIR" "$FLEETDECK_PORT" > ${JSON.stringify(marker)}; ` +
      `exec env -u FLEETDECK_SPAWN_CMD FLEETDECK_HOME="$SCRATCH_HOME" FLEETDECK_PORT="$FLEETDECK_PORT" FLEETDECK_TMUX_SOCKET="$FLEETDECK_TMUX_SOCKET" node ${JSON.stringify(FLEETD)}`,
    ], {
      env: { ...env, CLONE_SETUP: setupFile },
      stdio: ['ignore', 'ignore', 'pipe'],
    });
    const run = {
      label,
      socket,
      daemon: shell,
      setupFile,
      marker,
      scratch: '',
      project: '',
      cloneProj,
      root,
      port: 0,
      state: null,
      stderr: '',
    };
    shell.stderr.on('data', (chunk) => { run.stderr += chunk; });
    runs.push(run);

    // The sourced setup ran inside the shell: recover its mktemp paths from
    // the marker file, then wait for the daemon to report spawn.available.
    const deadline = Date.now() + 20000;
    while (Date.now() < deadline) {
      if (shell.exitCode !== null) break;
      if (!run.port && existsSync(marker)) {
        const [scratch, project, port] = readFileSync(marker, 'utf8').trim().split('\n');
        if (scratch && project && port) {
          run.scratch = scratch;
          run.project = project;
          run.port = Number(port);
        }
      }
      if (run.port) {
        try {
          const res = await fetch(`http://127.0.0.1:${run.port}/state`, { signal: AbortSignal.timeout(500) });
          const state = await res.json();
          if (state?.spawn?.available === true) {
            run.state = state;
            return run;
          }
        } catch { /* not ready yet */ }
      }
      await new Promise((resolve) => setTimeout(resolve, 200));
    }
    throw new Error(`run ${label}: daemon never became ready (exit ${shell.exitCode})${run.stderr ? ` -- stderr: ${run.stderr.trim()}` : ''}`);
  };

  try {
    const [runA, runB] = await Promise.all([startRun('a'), startRun('b')]);

    // Unique resources: distinct ports, homes, fixture copies, tmux sockets.
    assert.notEqual(runA.port, runB.port, 'both runs bound the same port');
    assert.notEqual(runA.scratch, runB.scratch, 'both runs share a daemon home');
    assert.notEqual(runA.project, runB.project, 'both runs share a fixture copy');
    assert.ok(!runA.scratch.includes('.fleetdeck-test') && !runB.scratch.includes('.fleetdeck-test'));
    for (const run of [runA, runB]) {
      assert.ok(existsSync(path.join(run.project, '.seed/util.js')), `${run.label}: fixture copy missing seeds`);
      assert.ok(existsSync(path.join(run.project, '.claude')), `${run.label}: fixture copy missing .claude dir`);
      assert.ok(run.state.spawn.available === true, `${run.label}: daemon did not report spawn.available`);
    }

    // Simulate run A's gate finishing and cleaning up: kill its daemon and
    // its scoped tmux server exactly as cleanup_resources does (|| true:
    // the server may already be gone with the daemon).
    runA.daemon.kill('SIGTERM');
    try { execFileSync('tmux', ['-L', runA.socket, 'kill-server'], { stdio: 'ignore' }); } catch { /* already gone */ }
    await new Promise((resolve) => setTimeout(resolve, 500));

    // Run B must be untouched: daemon still answering with spawn available.
    const res = await fetch(`http://127.0.0.1:${runB.port}/state`, { signal: AbortSignal.timeout(1000) });
    const stateB = await res.json();
    assert.equal(stateB.spawn?.available, true, 'run B daemon died or lost spawn capability when run A cleaned up');
    assert.equal(runB.daemon.exitCode, null, 'run B daemon process exited when run A cleaned up');
    assert.ok(existsSync(path.join(runB.scratch, 'fleetd.pid')), 'run B scratch home was removed by run A');
  } finally {
    cleanup();
  }
});
