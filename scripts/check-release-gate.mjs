// scripts/check-release-gate.mjs — semantic-version gate for the complete
// behavior-bearing plugin payload.
//
// Hooks run on every installed machine, while commands and skills become model
// instructions. Claude Code's marketplace cache key is the explicit plugin
// VERSION, not a source ref or pathname: editing a
// manifest's description while leaving the version alone used to satisfy the
// old "a version-bearing file changed" predicate even though the cache key did
// not move. First-time and existing installs could then receive different
// behavior under the same version.
//
// This checker is the root-cause fix: when the watched plugin payload changed
// between BASE and HEAD, it resolves the plugin version from BOTH revisions
// and requires
//   1. the resolved plugin version to increase semantically (semver
//      major/minor/patch compare — never inferred from a changed filename), and
//   2. every release manifest at HEAD to equal that new version (package.json,
//      .claude-plugin/plugin.json, .claude-plugin/marketplace.json, and
//      board/package.json). bun.lock has no root version field, so consistency
//      with the manifests is enforced by `bun install --frozen-lockfile`.
//
// Usage: node scripts/check-release-gate.mjs <base-ref> [head-ref]
// Exit 0 when the closure is untouched or every requirement holds; exit 1
// with a diagnostic otherwise. Run by the hook-integrity CI job for pull
// requests and main pushes.

import { execFileSync } from 'node:child_process';
import {
  PLUGIN_PAYLOAD_PATHS,
  VERSION_MANIFEST_PATHS,
  versionFromManifest,
} from './release-contract.mjs';

function git(args) {
  return execFileSync('git', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
}

function readJsonAt(ref, file) {
  try {
    return JSON.parse(git(['show', `${ref}:${file}`]));
  } catch {
    return null;
  }
}

// Claude Code resolves the plugin version from plugin.json when present,
// falling back to the marketplace entry.
function pluginVersionAt(ref) {
  return (
    readJsonAt(ref, '.claude-plugin/plugin.json')?.version ??
    readJsonAt(ref, '.claude-plugin/marketplace.json')?.plugins?.[0]?.version ??
    null
  );
}

function parseSemver(v) {
  const m = /^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$/.exec(v ?? '');
  if (!m) return null;
  return { major: Number(m[1]), minor: Number(m[2]), patch: Number(m[3]), pre: m[4] ?? null };
}

// Returns true when `head` is semantically greater than `base`.
export function semverGt(head, base) {
  const h = parseSemver(head);
  const b = parseSemver(base);
  if (!h || !b) return false;
  for (const k of ['major', 'minor', 'patch']) {
    if (h[k] !== b[k]) return h[k] > b[k];
  }
  // A release supersedes its prereleases (1.0.0 > 1.0.0-rc.1); prerelease vs
  // prerelease is not ordered here — require a numeric field to move.
  return h.pre === null && b.pre !== null;
}

export function checkReleaseGate({ base, head = 'HEAD' }) {
  const changed = git(['diff', '--name-only', base, head, '--', ...PLUGIN_PAYLOAD_PATHS]);
  if (!changed) {
    return { ok: true, lines: ['✓ plugin payload untouched'] };
  }

  const lines = ['plugin payload changed:', changed];
  const fail = (msg) => ({ ok: false, lines: [...lines, `::error::${msg}`] });

  const baseVersion = pluginVersionAt(base);
  const headVersion = pluginVersionAt(head);
  if (!headVersion) {
    return fail('could not resolve the plugin version at HEAD (.claude-plugin/plugin.json or marketplace.json)');
  }
  if (!baseVersion) {
    return fail(`could not resolve the plugin version at base ${base} — cannot prove an increase`);
  }
  if (!semverGt(headVersion, baseVersion)) {
    return fail(
      `the behavior-bearing plugin payload changed but the plugin version did not increase (${baseVersion} -> ${headVersion}) — ` +
      'the plugin version is Claude Code\'s install cache key, so behavior-bearing plugin changes must ride a real version bump, not just a manifest edit'
    );
  }

  for (const file of VERSION_MANIFEST_PATHS) {
    const v = versionFromManifest(file, readJsonAt(head, file) ?? {});
    if (v !== headVersion) {
      return fail(`version drift at HEAD — plugin version is ${headVersion} but ${file} says ${v ?? '(missing)'}`);
    }
  }

  return {
    ok: true,
    lines: [...lines, `✓ plugin payload changed alongside a version bump (${baseVersion} -> ${headVersion}); all manifests agree`],
  };
}

function isMain() {
  return process.argv[1] && import.meta.url === new URL(`file://${process.argv[1]}`).href;
}

if (isMain()) {
  const base = process.argv[2];
  const head = process.argv[3] ?? 'HEAD';
  if (!base) {
    console.error('usage: node scripts/check-release-gate.mjs <base-ref> [head-ref]');
    process.exit(2);
  }
  const result = checkReleaseGate({ base, head });
  for (const line of result.lines) console.log(line);
  process.exit(result.ok ? 0 : 1);
}
