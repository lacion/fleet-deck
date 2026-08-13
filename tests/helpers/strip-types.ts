// tests/helpers/strip-types.ts
//
// Cross-runtime TypeScript type-stripper for the source-slice tests.
//
// A few tests read a slice of a shipped .tsx/.ts source file and eval it with
// new Function() so the assertion pins the SHIPPED code, not a re-implementation
// (board-util's OSC 52 clipboard provider). The slice is TypeScript, so its type
// annotations must be erased before new Function() parses it as plain JS.
//
// Node exposes Module.stripTypeScriptTypes (a builtin since 22.6); Bun does not,
// but ships Bun.Transpiler. A static `import { stripTypeScriptTypes }` is a hard
// load-time SyntaxError under Bun (the named export is absent), so we import the
// namespace — which never throws — and dispatch on what the runtime actually has.
// Under Node this calls the very same builtin as before, so it is byte-identical.

import * as nodeModule from 'node:module';

type Stripper = (code: string) => string;

type BunTranspilerCtor = new (options: {
  loader: string;
}) => { transformSync(code: string): string };

/** Erase TypeScript annotations from a source fragment, on Node or Bun. */
export function stripTypes(src: string): string {
  const nodeStrip = (nodeModule as { stripTypeScriptTypes?: Stripper }).stripTypeScriptTypes;
  if (typeof nodeStrip === 'function') return nodeStrip(src);
  const bun = (globalThis as { Bun?: { Transpiler: BunTranspilerCtor } }).Bun;
  if (bun) return new bun.Transpiler({ loader: 'ts' }).transformSync(src);
  throw new Error('strip-types: no TypeScript type-stripper in this runtime');
}
