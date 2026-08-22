// Post-bundle gzip-headroom lever for src/daemon/fleetd.bundle.mjs.
//
// Runs AFTER esbuild + the @__PURE__ strip stage in `bun run bundle`. It is a
// tokenizer-grade JS lexer that removes only two byte-classes from the generated
// ESM, never touching code, string literals, template literals, or regexes:
//
//   1. JSDoc block comments — a block comment whose third character is `*`
//      (`/**`). Replaced by an equal number of newlines so the artifact's total
//      LINE COUNT is preserved verbatim (the policy gate asserts > 10_000 lines).
//   2. esbuild file-path header line comments at column 0
//      (`// node_modules/effect/…/foo.js`). Blanked to an empty line. This costs
//      per-module file attribution in the bundle, so it is applied only because
//      the JSDoc pass ALONE leaves < 1.5 KB of gzip headroom under the ceiling.
//
// Deliberately preserved: the shebang, the two-line GENERATED banner, non-JSDoc
// block comments (e.g. `/* v8 ignore next 2 */`), and every `//` comment that is
// not a column-0 esbuild path header.
//
// The target path is REQUIRED as argv[2] and never defaulted: the packed-smoke
// verifier (scripts/effect-migration/p3-packed-install-smoke.ts) rewrites every
// `src/daemon/fleetd.bundle.mjs` literal in the recipe tail to a scratch copy,
// so this stage must operate on whatever path it is handed.

import { readFileSync, writeFileSync } from 'node:fs';

const NL = 10; // \n
const SLASH = 47; // /
const STAR = 42; // *
const BACKSLASH = 92; // \
const SINGLE = 39; // '
const DOUBLE = 34; // "
const BACKTICK = 96; // `
const DOLLAR = 36; // $
const LBRACE = 123; // {
const RBRACE = 125; // }
const LBRACKET = 91; // [
const RBRACKET = 93; // ]
const RPAREN = 41; // )
const SPACE = 32;
const TAB = 9;
const CR = 13;
const HASH = 35; // #
const BANG = 33; // !

// A column-0 esbuild file header: `// ` then a single space-free path token that
// ends in a source extension. The GENERATED banner has interior spaces after
// `// `, so `[^ ]+` never matches it — the banner is preserved.
const HEADER_RE = /^\/\/ [^ ]+\.(?:js|jsx|ts|tsx|mjs|cjs)$/;

// Keywords after which a `/` opens a regex literal, not a division.
const KEYWORDS_BEFORE_REGEX = new Set([
  'return',
  'typeof',
  'instanceof',
  'in',
  'of',
  'new',
  'delete',
  'void',
  'throw',
  'case',
  'do',
  'else',
  'yield',
  'await',
]);

const DIAGNOSTIC_NAMES = [
  '"DaemonHostControl"',
  '"LifecycleCoordinator"',
  '"BunProcessDriver"',
  '"makeDaemonApp"',
] as const;

export interface TransformOptions {
  readonly stripJsdoc: boolean;
  readonly stripHeaders: boolean;
}

export interface TransformResult {
  readonly code: string;
  readonly jsdocBlocks: number;
  readonly jsdocLines: number;
  readonly headerLines: number;
}

type TokenKind = 'block' | 'line' | 'regex' | 'div' | 'string' | 'template' | 'ident' | 'other';

interface Token {
  readonly end: number; // index just past the token
  readonly regexOk: boolean; // may a `/` immediately after this token open a regex?
  readonly kind: TokenKind;
}

function isIdentStart(code: number): boolean {
  return (
    (code >= 65 && code <= 90) || // A-Z
    (code >= 97 && code <= 122) || // a-z
    code === 95 || // _
    code === DOLLAR ||
    code > 127 // non-ASCII identifier characters
  );
}

function isIdentPart(code: number): boolean {
  return isIdentStart(code) || (code >= 48 && code <= 57); // 0-9
}

function isRegexFlag(code: number): boolean {
  return (code >= 97 && code <= 122) || (code >= 65 && code <= 90);
}

function isWhitespace(code: number): boolean {
  return code === SPACE || code === TAB || code === NL || code === CR;
}

/**
 * Strip JSDoc block comments and/or esbuild path headers from generated ESM.
 *
 * The whole file is walked with a single-pass lexer so that `/**` sequences and
 * `// path` lines that live inside strings, template literals, or regexes are
 * never mistaken for comments. Strings/templates/regexes are always re-emitted
 * byte-for-byte; only genuine code-context comments are rewritten.
 */
export function transform(src: string, opts: TransformOptions): TransformResult {
  const n = src.length;
  const out: string[] = [];
  let i = 0;
  let regexOk = true; // does a `/` here open a regex literal?
  let jsdocBlocks = 0;
  let jsdocLines = 0;
  let headerLines = 0;

  const cc = (k: number): number => src.charCodeAt(k); // NaN when out of range

  // Find the index just past a `*/` starting at a `/*`.
  function scanBlockEnd(start: number): number {
    let j = start + 2;
    while (j < n && !(cc(j) === STAR && cc(j + 1) === SLASH)) j += 1;
    return j < n ? j + 2 : n;
  }

  // Find the index of the terminating newline (or EOF) of a `//` comment.
  function scanLineEnd(start: number): number {
    let j = start + 2;
    while (j < n && cc(j) !== NL) j += 1;
    return j;
  }

  // Find the index just past the closing quote of a string literal. Bounded to
  // the current line — a raw newline in a single/double-quoted string is invalid
  // JS, so stopping there prevents a malformed input from swallowing the file.
  function scanString(start: number, quote: number): number {
    let j = start + 1;
    while (j < n) {
      const d = cc(j);
      if (d === BACKSLASH) {
        j += 2;
        continue;
      }
      if (d === quote) return j + 1;
      if (d === NL) return j;
      j += 1;
    }
    return j;
  }

  // Find the index just past the matching `}` of a `${ … }` interpolation, given
  // the index right after `${`. Tokenizes the expression with the same `classify`
  // used at top level so nested strings, templates, comments, AND regex literals
  // (e.g. `${e.replace(/'/g, "…")}`) never miscount braces or leak state.
  function scanInterpolation(start: number): number {
    let j = start;
    let depth = 0;
    let regexOk = true; // just after `${` an expression is expected
    while (j < n) {
      const c = cc(j);
      if (isWhitespace(c)) {
        j += 1;
        continue;
      }
      if (c === RBRACE && depth === 0) return j + 1;
      if (c === LBRACE) depth += 1;
      else if (c === RBRACE) depth -= 1;
      const token = classify(j, regexOk);
      if (token.kind !== 'block' && token.kind !== 'line') regexOk = token.regexOk;
      j = token.end;
    }
    return j;
  }

  // Find the index just past the closing backtick of a template literal.
  function scanTemplate(start: number): number {
    let j = start + 1;
    while (j < n) {
      const d = cc(j);
      if (d === BACKSLASH) {
        j += 2;
        continue;
      }
      if (d === BACKTICK) return j + 1;
      if (d === DOLLAR && cc(j + 1) === LBRACE) {
        j = scanInterpolation(j + 2);
        continue;
      }
      j += 1;
    }
    return j;
  }

  // Find the index just past the trailing flags of a regex literal.
  function scanRegex(start: number): number {
    let j = start + 1;
    let inClass = false;
    while (j < n) {
      const d = cc(j);
      if (d === BACKSLASH) {
        j += 2;
        continue;
      }
      if (d === NL) break; // unterminated regex — bounded to its line
      if (d === LBRACKET) inClass = true;
      else if (d === RBRACKET) inClass = false;
      else if (d === SLASH && !inClass) {
        j += 1;
        break;
      }
      j += 1;
    }
    while (j < n && isRegexFlag(cc(j))) j += 1;
    return j;
  }

  // Classify the single token starting at `j` (which must not be whitespace),
  // given whether a `/` here would open a regex. This is the one source of truth
  // for tokenization, shared by the top-level walk and `scanInterpolation`. For
  // comments `regexOk` is returned unchanged (they are transparent).
  function classify(j: number, regexOk: boolean): Token {
    const c = cc(j);
    if (c === SLASH) {
      const next = cc(j + 1);
      if (next === STAR) return { end: scanBlockEnd(j), regexOk, kind: 'block' };
      if (next === SLASH) return { end: scanLineEnd(j), regexOk, kind: 'line' };
      if (regexOk) return { end: scanRegex(j), regexOk: false, kind: 'regex' };
      return { end: j + 1, regexOk: true, kind: 'div' };
    }
    if (c === SINGLE || c === DOUBLE) {
      return { end: scanString(j, c), regexOk: false, kind: 'string' };
    }
    if (c === BACKTICK) {
      return { end: scanTemplate(j), regexOk: false, kind: 'template' };
    }
    if (isIdentStart(c)) {
      let k = j + 1;
      while (k < n && isIdentPart(cc(k))) k += 1;
      return { end: k, regexOk: KEYWORDS_BEFORE_REGEX.has(src.slice(j, k)), kind: 'ident' };
    }
    // Any other single character (operators, punctuation, digits, braces). A `/`
    // after `)` or `]` is division; after anything else (incl. `{`/`}`) a regex.
    return { end: j + 1, regexOk: !(c === RPAREN || c === RBRACKET), kind: 'other' };
  }

  // Preserve the shebang line verbatim, if present.
  if (cc(0) === HASH && cc(1) === BANG) {
    const nl = src.indexOf('\n');
    const end = nl === -1 ? n : nl + 1;
    out.push(src.slice(0, end));
    i = end;
  }

  while (i < n) {
    const c = cc(i);

    // Whitespace run — transparent to regex/division disambiguation.
    if (isWhitespace(c)) {
      let j = i + 1;
      while (j < n && isWhitespace(cc(j))) j += 1;
      out.push(src.slice(i, j));
      i = j;
      continue;
    }

    const token = classify(i, regexOk);

    if (token.kind === 'block') {
      // Strip JSDoc (`/**`), keep every other block comment. Comments are
      // transparent to regexOk, so it is left unchanged.
      if (opts.stripJsdoc && cc(i + 2) === STAR) {
        const commentBody = src.slice(i, token.end);
        let lines = 0;
        for (let k = 0; k < commentBody.length; k += 1) {
          if (commentBody.charCodeAt(k) === NL) lines += 1;
        }
        out.push(lines > 0 ? '\n'.repeat(lines) : '');
        jsdocBlocks += 1;
        jsdocLines += lines;
      } else {
        out.push(src.slice(i, token.end));
      }
    } else if (token.kind === 'line') {
      // Blank column-0 esbuild path headers, keep every other line comment.
      const comment = src.slice(i, token.end);
      const atCol0 = i === 0 || cc(i - 1) === NL;
      if (opts.stripHeaders && atCol0 && HEADER_RE.test(comment)) {
        out.push('');
        headerLines += 1;
      } else {
        out.push(comment);
      }
    } else {
      out.push(src.slice(i, token.end));
      regexOk = token.regexOk;
    }

    i = token.end;
  }

  return { code: out.join(''), jsdocBlocks, jsdocLines, headerLines };
}

function main(): void {
  const target = process.argv[2];
  if (target === undefined || target.length === 0) {
    throw new Error('strip-bundle-jsdoc: target bundle path is required as argv[2]');
  }

  const original = readFileSync(target, 'utf8');
  const result = transform(original, { stripJsdoc: true, stripHeaders: true });
  const { code } = result;

  // ── Self-audit — abort the build if any invariant is violated ──────────────
  const inLines = original.split('\n').length;
  const outLines = code.split('\n').length;
  if (outLines !== inLines) {
    throw new Error(`strip-bundle-jsdoc: line count changed ${inLines} -> ${outLines}`);
  }
  for (const name of DIAGNOSTIC_NAMES) {
    if (!code.includes(name)) {
      throw new Error(`strip-bundle-jsdoc: diagnostic name ${name} did not survive`);
    }
  }
  const head = code.split('\n', 3);
  if (head[0] === undefined || !head[0].startsWith('#!/usr/bin/env bun')) {
    throw new Error('strip-bundle-jsdoc: shebang missing after transform');
  }
  if (head[1] === undefined || !head[1].startsWith("// GENERATED by 'bun run bundle'")) {
    throw new Error('strip-bundle-jsdoc: GENERATED banner missing after transform');
  }
  const second = transform(code, { stripJsdoc: true, stripHeaders: true });
  if (second.code !== code) {
    throw new Error('strip-bundle-jsdoc: transform is not idempotent');
  }

  writeFileSync(target, code);

  const savedBytes = Buffer.byteLength(original, 'utf8') - Buffer.byteLength(code, 'utf8');
  process.stdout.write(
    `strip-bundle-jsdoc: stripped ${result.jsdocBlocks} JSDoc blocks ` +
      `(${result.jsdocLines} lines) + ${result.headerLines} file headers, ` +
      `saved ${savedBytes} raw bytes, line count ${outLines} preserved\n`,
  );
}

if (import.meta.main) main();
