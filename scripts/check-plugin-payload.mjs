#!/usr/bin/env node
// scripts/check-plugin-payload.mjs — the release-bound plugin payload gate.
//
// The marketplace plugin cache keys on the plugin.json version: an unchanged
// explicit version is skipped as "already latest", so every byte of the
// behavior-bearing payload that ships under that version must ride a release.
// Before this verifier existed, the hook-integrity CI job watched only seven
// hook-side paths — a change to src/daemon/http.mjs plus a rebuilt
// fleetd.bundle.mjs (or board source plus rebuilt board-dist) passed every
// gate with the version untouched, and existing installs kept the old cached
// payload while new installs got different behavior under the same semantic
// version (BUG-001).
//
// release-contract.mjs is the single checked-in definition of the payload
// closure. This verifier fails (exit 1) when any payload path changed between
// BASE and HEAD without a version-manifest change in the same range.
//
//   node scripts/check-plugin-payload.mjs <base-ref> [head-ref]
//
// Exit 0 also when there is no base to compare (branch creation, tag push).

import { execFileSync } from 'node:child_process';
import { PLUGIN_PAYLOAD_PATHS, VERSION_MANIFEST_PATHS } from './release-contract.mjs';

// The complete behavior-bearing plugin payload:
//  - hooks/ + the hook scripts: run on every installed machine at every
//    SessionStart / tool call with the user's full environment.
//  - commands/, skills/, and agents/: prompt/instruction payload loaded into
//    Claude Code; changes here alter behavior just as surely as hook code.
//  - bin/: the CLI every install shares.
//  - src/daemon/: ALL daemon sources AND the two cached artifacts built
//    from them (fleetd.bundle.mjs is what fleet-sessionstart.mjs actually
//    execs; board-dist/ is what http.mjs serves). Watching the whole
//    directory means a NEW daemon module cannot slip in unwatched either.
//  - board/: the board source and lockfile that produce board-dist.
//  - .claude-plugin/: plugin.json is the cache key itself; marketplace.json
//    repeats it.
// A change to any of these signals a release. The version CI job already
// proves the four agree on the version string, so any one of them changing
// means the closure is riding a release.

const base = process.argv[2];
const head = process.argv[3] ?? 'HEAD';

if (!base || base === '0000000000000000000000000000000000000000') {
  console.log('no base to compare — skipping');
  process.exit(0);
}

function git(args) {
  try {
    return execFileSync('git', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
  } catch {
    return '';
  }
}

// Resolve/fetch the base in CI shallow clones; absent locally is fine too.
if (!git(['rev-parse', '--verify', '--quiet', `${base}^{commit}`])) {
  git(['fetch', '--no-tags', 'origin', base]);
}

const changed = git(['diff', '--name-only', base, head, '--', ...PLUGIN_PAYLOAD_PATHS]);
if (!changed) {
  console.log('✓ plugin payload closure untouched');
  process.exit(0);
}

console.log('plugin payload closure changed:');
console.log(changed);

const versionChanged = git(['diff', '--name-only', base, head, '--', ...VERSION_MANIFEST_PATHS]);
if (!versionChanged) {
  console.error(
    '::error::the behavior-bearing plugin payload changed but no version manifest did — ' +
    'the marketplace cache keys on the plugin version, so payload changes must ride a release'
  );
  process.exit(1);
}

console.log('✓ plugin payload changed alongside a version bump (release-bound)');
