// tests/import-boundaries.test.ts
//
// Architecture tripwire for the module zones the Bun-primary layout pins:
//
//   contracts/     — the shared type/validator spine (imported by everyone)
//   src/daemon/    — domain/compatibility code plus the Effect application and
//                    Bun platform zones introduced by the P2 migration
//   board/src/     — the Vite SPA (browser bundle; no daemon runtime, no bun:*)
//   scripts/fleet-*.ts — the FAIL-OPEN hook floor (must stay a thin edge onto a
//                    named seam of src/daemon, never drag the whole server in)
//
// These are static invariants over the *actual* sanctioned edges. The P2 checks
// additionally pin the framework-free zones, native implementation selection,
// v4-only vocabulary, unstable-import register, and generated board/hook output.
// Biome mirrors the import edges for earlier feedback, while this suite remains
// the stronger anchor: a lint suppression cannot silence a green-or-revert test.
//
// The predicates are pure and self-validating: the final `meta` test drives
// synthetic (importer, specifier) pairs through the exact functions the file
// walk uses, asserting they BOTH catch real violations and clear the legit
// edges — so this tripwire provably can trip. A boundary test that can never
// fail is worse than none.

import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import * as ts from 'typescript';

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const DAEMON = path.join(REPO, 'src', 'daemon');
const BOARD = path.join(REPO, 'board');
const BOARD_SRC = path.join(REPO, 'board', 'src');
const CONTRACTS = path.join(REPO, 'contracts');
const APP = path.join(DAEMON, 'app');
const LIVE_LAYER = path.join(APP, 'live-layer.ts');
const BOOTSTRAP_PROCESS_RUNTIME = path.join(APP, 'bootstrap-process-runtime.ts');
const PLATFORM = path.join(DAEMON, 'platform');
const UNSTABLE_IMPORTS_REGISTER = path.join(
  REPO,
  'docs',
  'v1',
  'evidence',
  'effect',
  'unstable-imports.md',
);
const APPROVED_EFFECT_RC = '4.0.0-rc.110';

const V3_PACKAGES = ['@effect/platform', '@effect/sql', '@effect/experimental'] as const;
const NATIVE_EFFECT_PACKAGES = ['@effect/platform-bun', '@effect/sql-sqlite-bun'] as const;
const LEGACY_EFFECT_MEMBERS = {
  Context: ['Tag', 'GenericTag'],
  Effect: [
    'Tag',
    'Service',
    'async',
    'asyncEffect',
    'fork',
    'forkDaemon',
    'catchAll',
    'catchAllCause',
    'runPromise',
    'runFork',
  ],
  Layer: ['scoped', 'scopedContext'],
  Scope: ['extend'],
} as const;
type EffectNamespace = keyof typeof LEGACY_EFFECT_MEMBERS;

// The daemon modules the fail-open floor (scripts/fleet-*.ts) is sanctioned to
// import — the config/nonce/env-scrub/takeover seam it uses to agree with a
// running daemon. Growing this set is a deliberate act: widening the floor's
// dependency surface must be a conscious edit here, which is the whole point.
const FLOOR_FILES = ['fleet-hook.ts', 'fleet-sessionstart.ts', 'fleet-watch.ts'].map((f) =>
  path.join(REPO, 'scripts', f),
);
const FLOOR_SEAM_ALLOW = new Set(['config', 'run-nonce', 'env-scrub', 'takeover']);
const HOOK_ARTIFACTS = fs
  .readdirSync(path.join(REPO, 'scripts'), { withFileTypes: true })
  .filter((entry) => entry.isFile() && /^fleet-.*\.mjs$/.test(entry.name))
  .map((entry) => path.join(REPO, 'scripts', entry.name))
  .sort();

// Runtime-only deps that belong to the daemon and must never reach the browser
// bundle. `effect` is the named board tripwire ([[drive-and-observe-provider]]);
// kysely/hono are the other Step 4/5 daemon deps; `bun:*` builtins are
// nonsensical (and unresolvable) under Vite.
function isDaemonOnlyDep(bare: string): boolean {
  if (bare.startsWith('bun:')) return true;
  for (const dep of ['effect', '@effect', 'kysely', 'hono']) {
    if (bare === dep || bare.startsWith(`${dep}/`)) return true;
  }
  return false;
}

// Recursively collect .ts/.tsx SOURCE under a root, skipping generated/vendored
// trees (board-dist is built board assets that live under src/daemon).
function walkSources(root: string): string[] {
  const out: string[] = [];
  const skip = new Set(['node_modules', 'board-dist', 'dist', '.git', '.codegraph', 'coverage']);
  const stack = [root];
  while (stack.length) {
    const dir = stack.pop() as string;
    let entries: fs.Dirent[];
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const e of entries) {
      const full = path.join(dir, e.name);
      if (e.isDirectory()) {
        if (!skip.has(e.name)) stack.push(full);
      } else if (e.isFile() && /\.(ts|tsx)$/.test(e.name)) {
        out.push(full);
      }
    }
  }
  return out.sort();
}

// Generated assets intentionally are not part of the TypeScript source walk.
// This walker keeps the generated tripwire format-agnostic: JS, source maps,
// HTML, and future text-bearing assets all receive the same marker scan.
function walkGeneratedFiles(root: string): string[] {
  let stat: fs.Stats;
  try {
    stat = fs.statSync(root);
  } catch {
    return [];
  }
  if (stat.isFile()) return [root];
  if (!stat.isDirectory()) return [];

  const out: string[] = [];
  const stack = [root];
  while (stack.length) {
    const dir = stack.pop() as string;
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) stack.push(full);
      else if (entry.isFile()) out.push(full);
    }
  }
  return out.sort();
}

// Every literal module specifier a source file names: imports, re-exports,
// dynamic imports, and import types. Parsing the AST keeps examples/comments
// from registering as real dependencies. Computed dynamic imports remain out of
// scope because they do not expose a statically auditable edge.
function specifiersOf(file: string, source: string): string[] {
  const specs: string[] = [];
  const sourceFile = ts.createSourceFile(
    file,
    source,
    ts.ScriptTarget.Latest,
    false,
    file.endsWith('.tsx') ? ts.ScriptKind.TSX : ts.ScriptKind.TS,
  );
  function inspect(node: ts.Node): void {
    if (ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) {
      const spec = moduleSpecifierText(node);
      if (spec !== undefined) specs.push(spec);
    } else if (
      ts.isCallExpression(node) &&
      node.expression.kind === ts.SyntaxKind.ImportKeyword &&
      node.arguments.length === 1
    ) {
      const argument = node.arguments[0];
      if (argument && ts.isStringLiteralLike(argument)) specs.push(argument.text);
    } else if (
      ts.isImportTypeNode(node) &&
      ts.isLiteralTypeNode(node.argument) &&
      ts.isStringLiteralLike(node.argument.literal)
    ) {
      specs.push(node.argument.literal.text);
    }
    ts.forEachChild(node, inspect);
  }
  inspect(sourceFile);
  return specs;
}

function isUnder(abs: string, root: string): boolean {
  return abs === root || abs.startsWith(root + path.sep);
}

// Resolve a specifier relative to the importing file. Relative specifiers yield
// an absolute path; bare specifiers (packages / bun: builtins) yield the raw
// name. A discriminated union so each predicate narrows without a null dance.
type Resolved = { kind: 'rel'; abs: string } | { kind: 'bare'; bare: string };
function resolveSpec(fromFile: string, spec: string): Resolved {
  if (spec.startsWith('.')) return { kind: 'rel', abs: path.resolve(path.dirname(fromFile), spec) };
  return { kind: 'bare', bare: spec };
}

function isEffectPackage(spec: string): boolean {
  return (
    spec === 'effect' ||
    spec.startsWith('effect/') ||
    spec === '@effect' ||
    spec.startsWith('@effect/')
  );
}

function isV3Package(spec: string): boolean {
  return V3_PACKAGES.some((name) => spec === name || spec.startsWith(`${name}/`));
}

function isNativeEffectPackage(spec: string): boolean {
  return NATIVE_EFFECT_PACKAGES.some((name) => spec === name || spec.startsWith(`${name}/`));
}

// --- the three boundary predicates (shared by the walk tests and the meta test) ---

// src/daemon/** must never import up into board/**.
function daemonImportsBoard(fromFile: string, spec: string): boolean {
  const r = resolveSpec(fromFile, spec);
  return r.kind === 'rel' && isUnder(r.abs, BOARD);
}

// board/src/** must never import src/daemon/** nor any daemon-only dep.
function boardImportsDaemon(fromFile: string, spec: string): boolean {
  const r = resolveSpec(fromFile, spec);
  if (r.kind === 'rel') return isUnder(r.abs, DAEMON);
  return isDaemonOnlyDep(r.bare);
}

// scripts/fleet-*.ts may reach into src/daemon only via the seam allowlist.
function floorImportsForbiddenDaemon(fromFile: string, spec: string): boolean {
  const r = resolveSpec(fromFile, spec);
  if (r.kind !== 'rel' || !isUnder(r.abs, DAEMON)) return false;
  const base = path.basename(r.abs).replace(/\.tsx?$/, '');
  return !FLOOR_SEAM_ALLOW.has(base);
}

// Shared contracts stay framework-free and cannot reach composition/platform
// code. They may continue to name existing domain types while that legacy edge
// exists; P2 does not silently broaden the older boundary policy.
function contractsImportForbidden(fromFile: string, spec: string): boolean {
  const r = resolveSpec(fromFile, spec);
  if (r.kind === 'bare') return isEffectPackage(r.bare);
  return isUnder(r.abs, APP) || isUnder(r.abs, PLATFORM);
}

// Existing daemon root modules are the migration's domain/compatibility zone.
// fleetd.ts is the composition entrypoint, not a domain workflow, and app/** and
// platform/** have their own policies below.
function domainImportForbidden(fromFile: string, spec: string): boolean {
  const r = resolveSpec(fromFile, spec);
  return r.kind === 'rel' && (isUnder(r.abs, APP) || isUnder(r.abs, PLATFORM));
}

function isDomainSource(file: string): boolean {
  return (
    isUnder(file, DAEMON) &&
    !isUnder(file, APP) &&
    !isUnder(file, PLATFORM) &&
    file !== path.join(DAEMON, 'fleetd.ts')
  );
}

// A platform module may import its native dependencies to implement a service;
// selection/wiring belongs only to the production Live Layer.
function selectsNativeImplementation(fromFile: string, spec: string): boolean {
  if (fromFile === LIVE_LAYER || isUnder(fromFile, PLATFORM)) return false;
  const r = resolveSpec(fromFile, spec);
  if (r.kind === 'bare') return isNativeEffectPackage(r.bare);
  return isUnder(r.abs, PLATFORM);
}

function plainZoneImportsEffect(fromFile: string, spec: string): boolean {
  const r = resolveSpec(fromFile, spec);
  return r.kind === 'bare' && isEffectPackage(r.bare);
}

function scan(files: string[], predicate: (f: string, s: string) => boolean): string[] {
  const violations: string[] = [];
  for (const file of files) {
    const source = fs.readFileSync(file, 'utf8');
    for (const spec of specifiersOf(file, source)) {
      if (predicate(file, spec)) violations.push(`${path.relative(REPO, file)} → ${spec}`);
    }
  }
  return violations;
}

function allSourceFiles(): string[] {
  return walkSources(REPO);
}

function sourceLine(sourceFile: ts.SourceFile, node: ts.Node): number {
  return sourceFile.getLineAndCharacterOfPosition(node.getStart(sourceFile)).line + 1;
}

function moduleSpecifierText(
  node: ts.ImportDeclaration | ts.ExportDeclaration,
): string | undefined {
  const specifier = node.moduleSpecifier;
  return specifier && ts.isStringLiteralLike(specifier) ? specifier.text : undefined;
}

type NamespaceBinding = { module: EffectNamespace; local: string };

// Inspect real import bindings through the TypeScript AST so the API policy is
// alias-aware without ever matching prose, comments, or string contents.
function legacyEffectApiViolationsInSource(file: string, source: string): string[] {
  const sourceFile = ts.createSourceFile(
    file,
    source,
    ts.ScriptTarget.Latest,
    true,
    file.endsWith('.tsx') ? ts.ScriptKind.TSX : ts.ScriptKind.TS,
  );
  const namespaceBindings: NamespaceBinding[] = [];
  const rootNamespaces = new Set<string>();
  const violations: string[] = [];

  function report(node: ts.Node, api: string): void {
    violations.push(`${path.relative(REPO, file)}:${sourceLine(sourceFile, node)} → ${api}`);
  }

  function effectNamespace(spec: string): EffectNamespace | undefined {
    if (!spec.startsWith('effect/')) return undefined;
    const segment = spec.slice('effect/'.length);
    return Object.hasOwn(LEGACY_EFFECT_MEMBERS, segment) ? (segment as EffectNamespace) : undefined;
  }

  for (const statement of sourceFile.statements) {
    if (ts.isImportDeclaration(statement)) {
      const spec = moduleSpecifierText(statement);
      if (!spec) continue;
      const clause = statement.importClause;
      const namespace = effectNamespace(spec);
      const bindings = clause?.namedBindings;

      if (spec === 'effect' && bindings && ts.isNamespaceImport(bindings)) {
        rootNamespaces.add(bindings.name.text);
      }

      if (namespace && clause?.name) {
        namespaceBindings.push({ module: namespace, local: clause.name.text });
      }
      if (namespace && bindings && ts.isNamespaceImport(bindings)) {
        namespaceBindings.push({ module: namespace, local: bindings.name.text });
      }

      if (bindings && ts.isNamedImports(bindings)) {
        for (const element of bindings.elements) {
          const imported = (element.propertyName ?? element.name).text;
          const local = element.name.text;
          if (spec === 'effect' && Object.hasOwn(LEGACY_EFFECT_MEMBERS, imported)) {
            namespaceBindings.push({ module: imported as EffectNamespace, local });
          } else if (
            namespace &&
            (LEGACY_EFFECT_MEMBERS[namespace] as readonly string[]).includes(imported)
          ) {
            report(element, `${namespace}.${imported}`);
          }
        }
      }
    } else if (ts.isExportDeclaration(statement)) {
      const spec = moduleSpecifierText(statement);
      const namespace = spec ? effectNamespace(spec) : undefined;
      if (!namespace || !statement.exportClause || !ts.isNamedExports(statement.exportClause)) {
        continue;
      }
      for (const element of statement.exportClause.elements) {
        const exported = (element.propertyName ?? element.name).text;
        if ((LEGACY_EFFECT_MEMBERS[namespace] as readonly string[]).includes(exported)) {
          report(element, `${namespace}.${exported}`);
        }
      }
    }
  }

  function inspect(node: ts.Node): void {
    if (ts.isPropertyAccessExpression(node) && ts.isIdentifier(node.expression)) {
      for (const binding of namespaceBindings) {
        if (
          node.expression.text === binding.local &&
          (LEGACY_EFFECT_MEMBERS[binding.module] as readonly string[]).includes(node.name.text)
        ) {
          report(node.name, `${binding.module}.${node.name.text}`);
        }
      }
    } else if (ts.isQualifiedName(node) && ts.isIdentifier(node.left)) {
      for (const binding of namespaceBindings) {
        if (
          node.left.text === binding.local &&
          (LEGACY_EFFECT_MEMBERS[binding.module] as readonly string[]).includes(node.right.text)
        ) {
          report(node.right, `${binding.module}.${node.right.text}`);
        }
      }
    }

    // `import * as Fx from "effect"; Fx.Effect.async(...)` is uncommon but is
    // still an Effect namespace import and cannot bypass the v4 vocabulary.
    if (
      ts.isPropertyAccessExpression(node) &&
      ts.isPropertyAccessExpression(node.expression) &&
      ts.isIdentifier(node.expression.expression) &&
      rootNamespaces.has(node.expression.expression.text) &&
      Object.hasOwn(LEGACY_EFFECT_MEMBERS, node.expression.name.text)
    ) {
      const namespace = node.expression.name.text as EffectNamespace;
      if ((LEGACY_EFFECT_MEMBERS[namespace] as readonly string[]).includes(node.name.text)) {
        report(node.name, `${namespace}.${node.name.text}`);
      }
    }

    ts.forEachChild(node, inspect);
  }
  inspect(sourceFile);

  return [...new Set(violations)].sort();
}

function legacyEffectApiViolations(file: string): string[] {
  return legacyEffectApiViolationsInSource(file, fs.readFileSync(file, 'utf8'));
}

type RegistryRow = {
  exactImport: string;
  rationale: string;
  tests: string;
  reviewedRc: string;
  rollbackModule: string;
};

function unquoteMarkdownCode(value: string): string {
  const trimmed = value.trim();
  return trimmed.startsWith('`') && trimmed.endsWith('`') ? trimmed.slice(1, -1) : trimmed;
}

function isPlaceholder(value: string): boolean {
  return /^(?:-|--|—|none|n\/a|tbd)$/i.test(value.trim());
}

function parseUnstableImportRegistry(source: string): {
  rows: RegistryRow[];
  errors: string[];
} {
  const startMarker = '<!-- unstable-import-registry:start -->';
  const endMarker = '<!-- unstable-import-registry:end -->';
  const start = source.indexOf(startMarker);
  const end = source.indexOf(endMarker);
  const errors: string[] = [];
  if (start < 0 || end < 0 || end < start) {
    return { rows: [], errors: ['registry start/end markers are missing or out of order'] };
  }

  const tableLines = source
    .slice(start + startMarker.length, end)
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.startsWith('|') && line.endsWith('|'));
  if (tableLines.length < 2) {
    return { rows: [], errors: ['registry must contain its five-column table header'] };
  }

  const header = tableLines[0]
    ?.slice(1, -1)
    .split('|')
    .map((cell) => cell.trim());
  const expectedHeader = [
    'Exact import',
    'Rationale',
    'Owning tests',
    'Last reviewed RC',
    'Rollback module',
  ];
  if (
    !header ||
    header.length !== expectedHeader.length ||
    !header.every((v, i) => v === expectedHeader[i])
  ) {
    errors.push(`registry header must be: ${expectedHeader.join(' | ')}`);
  }

  const rows: RegistryRow[] = [];
  for (const [offset, line] of tableLines.slice(2).entries()) {
    const cells = line.slice(1, -1).split('|').map(unquoteMarkdownCode);
    if (cells.length !== 5) {
      errors.push(`registry data row ${offset + 1} must have exactly five columns`);
      continue;
    }
    const [exactImport, rationale, tests, reviewedRc, rollbackModule] = cells;
    if (
      exactImport === undefined ||
      rationale === undefined ||
      tests === undefined ||
      reviewedRc === undefined ||
      rollbackModule === undefined
    ) {
      errors.push(`registry data row ${offset + 1} is incomplete`);
      continue;
    }
    rows.push({ exactImport, rationale, tests, reviewedRc, rollbackModule });
  }

  for (const [index, row] of rows.entries()) {
    const label = `registry data row ${index + 1}`;
    if (
      !/^effect\/unstable\/[A-Za-z0-9._/-]+$/.test(row.exactImport) ||
      row.exactImport.includes('*')
    ) {
      errors.push(`${label} must name one exact effect/unstable/* import (no wildcard)`);
    }
    for (const [field, value] of [
      ['rationale', row.rationale],
      ['owning tests', row.tests],
      ['rollback module', row.rollbackModule],
    ] as const) {
      if (value.trim() === '' || isPlaceholder(value))
        errors.push(`${label} needs a real ${field}`);
    }
    if (row.reviewedRc !== APPROVED_EFFECT_RC) {
      errors.push(`${label} last reviewed RC must be exactly ${APPROVED_EFFECT_RC}`);
    }
  }

  const imports = rows.map((row) => row.exactImport);
  if (new Set(imports).size !== imports.length) errors.push('registry contains duplicate imports');
  return { rows, errors };
}

type GeneratedMarker = { label: string; pattern: RegExp };
const GENERATED_EFFECT_MARKERS: readonly GeneratedMarker[] = [
  {
    label: 'external Effect module import',
    pattern:
      /\b(?:from\s*|import\s*(?:\(\s*)?|require\s*\(\s*)["'`](?:effect(?:\/[A-Za-z0-9_./-]+)?|@effect\/[A-Za-z0-9_./-]+)["'`]/,
  },
  {
    label: 'Effect runtime symbol',
    pattern: /\bSymbol\.for\(\s*["'`]effect\//,
  },
  {
    label: 'Effect core module marker',
    pattern: /["'`]effect\/(?:unstable\/)?[A-Z][A-Za-z0-9_-]*/,
  },
  {
    label: 'Effect platform module marker',
    pattern: /["'`]@effect\/[a-z0-9_-]+\/[A-Z][A-Za-z0-9_-]*/,
  },
  {
    label: 'Effect runtime TypeId',
    pattern: /\b(?:Effect|Layer|Scope|Fiber|Exit|Cause|Runtime|Context)TypeId\b/,
  },
  {
    label: 'Effect dependency metadata',
    pattern: /(?:\beffect|["'`]@effect\/[a-z0-9_-]+["'`])\s*:\s*["'`](?:[~^]|>=?|<=?)?\d+\.\d+/,
  },
];

function generatedEffectMarkersInSource(source: string): Array<{ label: string; line: number }> {
  const matches: Array<{ label: string; line: number }> = [];
  for (const marker of GENERATED_EFFECT_MARKERS) {
    const match = marker.pattern.exec(source);
    if (!match || match.index === undefined) continue;
    const line = source.slice(0, match.index).split(/\r?\n/).length;
    matches.push({ label: marker.label, line });
  }
  return matches;
}

function generatedEffectViolations(files: string[]): string[] {
  const violations: string[] = [];
  for (const file of files) {
    const source = fs.readFileSync(file, 'utf8');
    for (const marker of generatedEffectMarkersInSource(source)) {
      violations.push(`${path.relative(REPO, file)}:${marker.line} → ${marker.label}`);
    }
  }
  return violations.sort();
}

test('boundary: src/daemon never imports board/**', () => {
  const violations = scan(walkSources(DAEMON), daemonImportsBoard);
  assert.deepEqual(
    violations,
    [],
    `the daemon must not depend on the UI:\n${violations.join('\n')}`,
  );
});

test('boundary: board/src never imports src/daemon/** or a daemon-only dep', () => {
  const violations = scan(walkSources(BOARD_SRC), boardImportsDaemon);
  assert.deepEqual(
    violations,
    [],
    `the board talks to the daemon over HTTP, never by import (and never pulls effect/@effect/kysely/hono/bun:*):\n${violations.join('\n')}`,
  );
});

test('boundary: contracts stay Effect-free and cannot import app/platform composition', () => {
  const violations = scan(walkSources(CONTRACTS), contractsImportForbidden);
  assert.deepEqual(
    violations,
    [],
    `contracts must remain framework-free and independent of app/platform:\n${violations.join('\n')}`,
  );
});

test('boundary: the fail-open floor is Effect-free and reaches daemon only through its seam', () => {
  const violations = scan(
    FLOOR_FILES,
    (file, spec) => floorImportsForbiddenDaemon(file, spec) || plainZoneImportsEffect(file, spec),
  );
  assert.deepEqual(
    violations,
    [],
    `scripts/fleet-*.ts must stay Effect-free and a thin edge onto {${[...FLOOR_SEAM_ALLOW].join(', ')}}; widening the seam is a deliberate edit here:\n${violations.join('\n')}`,
  );
});

test('boundary: domain modules never import app or platform modules', () => {
  const domainFiles = walkSources(DAEMON).filter(isDomainSource);
  const violations = scan(domainFiles, domainImportForbidden);
  assert.deepEqual(
    violations,
    [],
    `domain/compatibility modules must not depend on composition or native adapters:\n${violations.join('\n')}`,
  );
});

test('boundary: native implementations are selected only in app/live-layer.ts', () => {
  const violations = scan(walkSources(DAEMON), selectsNativeImplementation);
  assert.deepEqual(
    violations,
    [],
    `only app/live-layer.ts may select a native platform implementation:\n${violations.join('\n')}`,
  );
});

test('policy: P3 has exactly one pre-root ManagedRuntime in its bootstrap bridge', () => {
  const daemonSources = walkSources(DAEMON);
  const importViolations = scan(
    daemonSources,
    (file, spec) => spec === 'effect/ManagedRuntime' && file !== BOOTSTRAP_PROCESS_RUNTIME,
  );
  assert.deepEqual(
    importViolations,
    [],
    `ManagedRuntime is permitted only in P3's disposable bootstrap bridge:\n${importViolations.join('\n')}`,
  );

  const bridgeSource = fs.readFileSync(BOOTSTRAP_PROCESS_RUNTIME, 'utf8');
  assert.ok(
    specifiersOf(BOOTSTRAP_PROCESS_RUNTIME, bridgeSource).includes('effect/ManagedRuntime'),
    'the P3 bootstrap bridge must own the sole ManagedRuntime import until P4 deletes it',
  );
  const bridgeAst = ts.createSourceFile(
    BOOTSTRAP_PROCESS_RUNTIME,
    bridgeSource,
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TS,
  );
  let managedRuntimeMakeCalls = 0;
  const visit = (node: ts.Node): void => {
    if (
      ts.isCallExpression(node) &&
      ts.isPropertyAccessExpression(node.expression) &&
      ts.isIdentifier(node.expression.expression) &&
      node.expression.expression.text === 'ManagedRuntime' &&
      node.expression.name.text === 'make'
    ) {
      managedRuntimeMakeCalls += 1;
    }
    ts.forEachChild(node, visit);
  };
  visit(bridgeAst);
  assert.equal(
    managedRuntimeMakeCalls,
    1,
    'the P3 bootstrap bridge must construct exactly one lazy ProcessRunner ManagedRuntime',
  );
});

test('policy: source rejects Effect v3 package names and APIs', () => {
  const sourceFiles = allSourceFiles();
  const packageViolations = scan(sourceFiles, (_file, spec) => isV3Package(spec));
  const apiViolations = sourceFiles.flatMap((file) => {
    const source = fs.readFileSync(file, 'utf8');
    return specifiersOf(file, source).some(isEffectPackage) ? legacyEffectApiViolations(file) : [];
  });
  assert.deepEqual(
    [...packageViolations, ...apiViolations],
    [],
    `the migration is v4-only; replace old packages/APIs with the RC.110 vocabulary:\n${[
      ...packageViolations,
      ...apiViolations,
    ].join('\n')}`,
  );
});

test('policy: every unstable Effect source import has a complete RC.110 registry row', () => {
  const registrySource = fs.readFileSync(UNSTABLE_IMPORTS_REGISTER, 'utf8');
  const registry = parseUnstableImportRegistry(registrySource);
  assert.deepEqual(
    registry.errors,
    [],
    `invalid unstable-import register:\n${registry.errors.join('\n')}`,
  );

  const sourceImports = new Set<string>();
  for (const file of allSourceFiles()) {
    const source = fs.readFileSync(file, 'utf8');
    for (const spec of specifiersOf(file, source)) {
      if (spec.startsWith('effect/unstable/')) sourceImports.add(spec);
    }
  }
  const registeredImports = new Set(registry.rows.map((row) => row.exactImport));
  const unregistered = [...sourceImports]
    .filter((specifier) => !registeredImports.has(specifier))
    .sort();
  const stale = [...registeredImports].filter((specifier) => !sourceImports.has(specifier)).sort();
  assert.deepEqual(
    { unregistered, stale },
    { unregistered: [], stale: [] },
    `unstable imports and registry rows must match exactly; add/remove the complete evidence row with the source change`,
  );
});

test('boundary: generated board and every hook artifact contain zero Effect runtime markers', () => {
  const boardArtifacts = [
    ...walkGeneratedFiles(path.join(DAEMON, 'board-dist')),
    ...walkGeneratedFiles(path.join(BOARD, 'dist')),
  ];
  assert.ok(
    boardArtifacts.length > 0,
    'the shipped generated board must exist for bundle scanning',
  );
  assert.ok(
    HOOK_ARTIFACTS.length > 0,
    'at least one generated fleet hook artifact must be scanned',
  );
  const artifacts = [...new Set([...boardArtifacts, ...HOOK_ARTIFACTS])].sort();
  const violations = generatedEffectViolations(artifacts);
  assert.deepEqual(
    violations,
    [],
    `generated board/hooks must not contain Effect imports or runtime fingerprints:\n${violations.join('\n')}`,
  );
});

// The tripwire must be able to trip: drive known-bad and known-good pairs
// through the exact predicates the walk uses.
test('boundary: the predicates catch violations and clear the sanctioned edges', () => {
  const daemonFile = path.join(DAEMON, 'http.ts');
  const boardFile = path.join(BOARD_SRC, 'components', 'Header.tsx');
  const floorFile = path.join(REPO, 'scripts', 'fleet-hook.ts');

  // daemon → board
  assert.equal(daemonImportsBoard(daemonFile, '../../board/src/util.ts'), true);
  assert.equal(daemonImportsBoard(daemonFile, './db.ts'), false);
  assert.equal(daemonImportsBoard(daemonFile, '../../contracts/index.ts'), false);
  assert.equal(daemonImportsBoard(daemonFile, '../../bin/tmux-version.ts'), false);

  // board → daemon / daemon-only dep
  assert.equal(boardImportsDaemon(boardFile, '../../../src/daemon/db.ts'), true);
  assert.equal(boardImportsDaemon(boardFile, 'effect'), true);
  assert.equal(boardImportsDaemon(boardFile, 'effect/Schema'), true);
  assert.equal(boardImportsDaemon(boardFile, '@effect/platform-bun/BunRuntime'), true);
  assert.equal(boardImportsDaemon(boardFile, 'bun:sqlite'), true);
  assert.equal(boardImportsDaemon(boardFile, '../../../contracts/index.ts'), false);
  assert.equal(boardImportsDaemon(boardFile, '../util.ts'), false);
  assert.equal(boardImportsDaemon(boardFile, 'react'), false);

  // floor → daemon seam
  assert.equal(floorImportsForbiddenDaemon(floorFile, '../src/daemon/http.ts'), true);
  assert.equal(floorImportsForbiddenDaemon(floorFile, '../src/daemon/db.ts'), true);
  assert.equal(floorImportsForbiddenDaemon(floorFile, '../src/daemon/config.ts'), false);
  assert.equal(floorImportsForbiddenDaemon(floorFile, '../src/daemon/takeover.ts'), false);
  assert.equal(floorImportsForbiddenDaemon(floorFile, '../contracts/index.ts'), false);

  // contracts / domain / platform composition
  const contractFile = path.join(CONTRACTS, 'state.ts');
  const domainFile = path.join(DAEMON, 'derive.ts');
  const appFile = path.join(APP, 'kernel.ts');
  const platformFile = path.join(PLATFORM, 'bun', 'process-runner-live.ts');
  assert.equal(contractsImportForbidden(contractFile, 'effect/Schema'), true);
  assert.equal(contractsImportForbidden(contractFile, '../src/daemon/app/kernel.ts'), true);
  assert.equal(contractsImportForbidden(contractFile, '../src/daemon/platform/bun/foo.ts'), true);
  assert.equal(contractsImportForbidden(contractFile, '../src/daemon/derive.ts'), false);
  assert.equal(domainImportForbidden(domainFile, './app/kernel.ts'), true);
  assert.equal(domainImportForbidden(domainFile, './platform/bun/foo.ts'), true);
  assert.equal(domainImportForbidden(domainFile, './db.ts'), false);
  assert.equal(selectsNativeImplementation(appFile, '../platform/bun/foo.ts'), true);
  assert.equal(selectsNativeImplementation(appFile, '@effect/platform-bun/BunRuntime'), true);
  assert.equal(selectsNativeImplementation(LIVE_LAYER, '../platform/bun/foo.ts'), false);
  assert.equal(selectsNativeImplementation(platformFile, '@effect/platform-bun/BunRuntime'), false);

  // package names are exact/prefix-aware: the v4 platform-bun package must not
  // be mistaken for the old @effect/platform package.
  assert.equal(isV3Package('@effect/platform'), true);
  assert.equal(isV3Package('@effect/platform/HttpServer'), true);
  assert.equal(isV3Package('@effect/sql'), true);
  assert.equal(isV3Package('@effect/experimental/Foo'), true);
  assert.equal(isV3Package('@effect/platform-bun'), false);

  // Generated scanning uses runtime/module fingerprints, not the substring
  // "effect": React, SVG, and benchmark script names are legitimate in the
  // board/hook bundles. Dependency metadata is not: hooks need only the Fleet
  // Deck version, so embedding package.json would leak the Effect cohort into a
  // deliberately plain artifact.
  assert.deepEqual(
    generatedEffectMarkersInSource(
      'useEffect(() => {}); const svg = "vector-effect"; const script = "effect:p0:probe";',
    ),
    [],
  );
  assert.ok(
    generatedEffectMarkersInSource(
      'const manifest = { effect: "4.0.0-rc.110", "@effect/platform-bun": "4.0.0-rc.110" };',
    ).some((marker) => marker.label === 'Effect dependency metadata'),
  );
  assert.ok(
    generatedEffectMarkersInSource('import { Effect } from "effect";').some(
      (marker) => marker.label === 'external Effect module import',
    ),
  );
  assert.ok(
    generatedEffectMarkersInSource('const id = Symbol.for("effect/Effect");').some(
      (marker) => marker.label === 'Effect runtime symbol',
    ),
  );

  // The API scanner follows aliases from both direct modules and the effect
  // barrel, and approved v4 names that merely share a prefix remain clear.
  const syntheticPolicyFile = path.join(REPO, 'synthetic-effect-policy.ts');
  assert.deepEqual(
    legacyEffectApiViolationsInSource(
      syntheticPolicyFile,
      'import * as E from "effect/Effect"; E.callback(() => {}); E.runPromiseWith(ctx);',
    ),
    [],
  );
  assert.deepEqual(
    legacyEffectApiViolationsInSource(
      syntheticPolicyFile,
      'import { Effect as E, Context as C } from "effect"; E.async(() => {}); C.Tag("Bad");',
    ).map((violation) => violation.replace(/^.* → /, '')),
    ['Context.Tag', 'Effect.async'],
  );
});
