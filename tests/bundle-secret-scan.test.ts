import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

// P11.6 D2 — a leak gate on the COMMITTED bundle artifacts. These `.mjs` files
// are the only fleetdeck code that ships verbatim (esbuild inlines everything),
// so a credential that reaches a bundle is a credential in the git history. The
// scan greps every committed artifact for high-signal secret LITERALS.
//
// The one false-positive risk is our own redaction machinery: payload-capture.ts
// ships regex SOURCES like `sk-ant-[A-Za-z0-9_-]{10,}` and the key name
// `FLEETDECK_TOKEN`. The discriminator is structural — a real secret has an
// alphanumeric character immediately after the prefix, whereas a regex source
// has a `[` (character class) or `\` there, and a key NAME is not a value. Every
// pattern below encodes that "alnum right after the prefix" shape, and the
// negative-control block proves the twins are rejected.

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

// Every committed bundle artifact. A rename/removal must fail loudly here rather
// than silently drop a file from the gate — hence the existence assertion.
const ARTIFACTS = [
  'bin/fleetdeck.mjs',
  'scripts/fleet-hook.mjs',
  'scripts/fleet-sessionstart.mjs',
  'scripts/fleet-watch.mjs',
  'src/daemon/fleetd.bundle.mjs',
] as const;

interface Pattern {
  readonly name: string;
  readonly re: RegExp;
}

// Global + case-sensitive. Each is anchored so the character AFTER the prefix
// must be alphanumeric — the property a redaction regex source (`[`, `\`) lacks.
const PATTERNS: readonly Pattern[] = [
  {
    name: 'pem-private-key',
    re: /-----BEGIN [A-Z0-9]{0,20}(?: [A-Z0-9]{1,20})* ?PRIVATE KEY-----/g,
  },
  { name: 'aws-access-key-id', re: /\bAKIA[0-9A-Z]{16}\b/g },
  { name: 'github-token', re: /\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{36}\b/g },
  { name: 'gitlab-pat', re: /\bglpat-[A-Za-z0-9][A-Za-z0-9_-]{19}\b/g },
  { name: 'anthropic-key', re: /\bsk-ant-[A-Za-z0-9][A-Za-z0-9_-]{20,}/g },
];

interface Hit {
  readonly pattern: string;
  readonly artifact: string;
  readonly index: number;
  readonly masked: string;
}

function scan(text: string, artifact: string): Hit[] {
  const hits: Hit[] = [];
  for (const { name, re } of PATTERNS) {
    re.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = re.exec(text)) !== null) {
      const found = m[0];
      // Never echo a real secret into CI logs: prefix + length only.
      hits.push({
        pattern: name,
        artifact,
        index: m.index,
        masked: `${found.slice(0, 8)}…(${found.length} chars)`,
      });
    }
  }
  return hits;
}

test('committed bundle artifacts contain no leaked secret literals (P11.6 D2)', () => {
  const hits: Hit[] = [];
  for (const rel of ARTIFACTS) {
    const text = readFileSync(path.join(ROOT, rel), 'utf8');
    hits.push(...scan(text, rel));
  }
  assert.deepEqual(
    hits,
    [],
    `secret literal(s) found in committed bundles:\n${hits
      .map((h) => `  ${h.artifact}@${h.index} [${h.pattern}] ${h.masked}`)
      .join('\n')}`,
  );
});

test('the secret scan bites — mutation and per-pattern positive controls (P11.6 D2)', () => {
  // Mutation: splice a fake GitHub token into a real bundle's content and prove
  // the scan catches it (the gate is wired to the same code path the real scan uses).
  const base = readFileSync(path.join(ROOT, 'bin/fleetdeck.mjs'), 'utf8');
  const fakeGhp = `ghp_${'a1B2c3D4e5'.repeat(3) + '012345'}`; // ghp_ + 36 alnum
  assert.equal(fakeGhp.length, 40);
  const mutated = `${base}\nconst leaked = "${fakeGhp}";\n`;
  const mutationHits = scan(mutated, 'bin/fleetdeck.mjs<mutated>');
  assert.equal(mutationHits.length, 1, 'exactly the injected token is caught');
  assert.equal(mutationHits[0]?.pattern, 'github-token');

  // Every pattern must be live (guard against a regex that silently never fires).
  const positives: Record<string, string> = {
    'pem-private-key': '-----BEGIN OPENSSH PRIVATE KEY-----',
    'aws-access-key-id': 'AKIAIOSFODNN7EXAMPLE',
    'github-token': fakeGhp,
    'gitlab-pat': `glpat-${'xY3'.repeat(6) + 'ab'}`, // glpat- + 20 chars
    'anthropic-key': `sk-ant-api03-${'Aa0'.repeat(8)}`,
  };
  for (const { name } of PATTERNS) {
    const sample = positives[name];
    assert.ok(sample, `positive sample defined for ${name}`);
    const found = scan(`prefix ${sample} suffix`, 'positive').filter((h) => h.pattern === name);
    assert.equal(found.length, 1, `${name} must match its positive sample`);
  }
});

test('the secret scan does not fire on redaction-regex sources or key names (P11.6 D2)', () => {
  // The exact shapes payload-capture.ts ships and that live in the bundles today:
  // regex SOURCES (bracket/backslash right after the prefix) and the config key
  // NAME. None is a secret value; none may match.
  const negatives = [
    'sk-ant-[A-Za-z0-9_-]{10,}', // SECRET_VALUE_RES anthropic source
    'AKIA[A-Z0-9]{16}', // aws source
    '(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{36}', // github source
    'glpat-[A-Za-z0-9_-]{20}', // gitlab source
    '-----BEGIN [A-Z ]{0,40}PRIVATE KEY-----', // pem source (space, not alnum, after BEGIN token)
    'FLEETDECK_TOKEN', // the key name, never the value
    'process.env.FLEETDECK_TOKEN',
  ];
  for (const n of negatives) {
    assert.deepEqual(scan(n, 'negative'), [], `must not flag: ${n}`);
  }
});
