// sqlite.ts — the runtime-agnostic SQLite handle fleetd opens its store through.
//
// The daemon runs on two runtimes and the SQLite driver differs by construction:
// the Claude Code plugin path forks the exact Node that Claude Code launched, so
// it gets node:sqlite (DatabaseSync); the standalone/dev path may run under bun,
// which ships bun:sqlite (Database) and has NO node:sqlite at all. Neither
// builtin exists on the other runtime, so a bare `import 'node:sqlite'` throws at
// link time under bun (and `import 'bun:sqlite'` throws under Node). This module
// is the single guarded seam: it picks the driver once, at import, off
// process.versions.bun, and every other file opens through openDatabase() so no
// other module names either builtin.
//
// The two drivers already agree on the tiny surface fleetd uses — positional `?`
// binding, multi-statement .exec(), plain-object rows, and a .run() result
// carrying { changes, lastInsertRowid } as plain numbers — verified against
// Node 22 and bun 1.3.14. The one observed divergence is a missed .get():
// node:sqlite returns `undefined`, bun:sqlite returns `null`. The wrapper below
// pins that to `undefined` so the two channels are byte-identical at the seam,
// and returns ONE stable object per open — statements.mjs keys a WeakMap on the
// handle to cache prepared statements, so the handle identity must stay durable
// for the life of the connection.

// The store's foundational value types. Every row shape db.ts and statements.ts
// declare is built on top of these: a cell is one of SQLite's storage classes, a
// row is a column-keyed record, and a caller asserts the concrete row shape via
// the `R` parameter of prepare() — the SQL text, not the driver, is what
// guarantees that shape, so the assertion belongs with each query, not here.
export type SqlValue = null | number | bigint | string | Uint8Array;
export type SqlRow = Record<string, SqlValue>;

// What .run() reports. Both drivers carry these as number | bigint (a rowid or
// change count past 2^53 stays exact only as a bigint), so any consumer doing
// arithmetic on them has to reckon with both.
export interface SqlRunResult {
  changes: number | bigint;
  lastInsertRowid: number | bigint;
}

// A prepared statement, generic over the row shape the caller asserts. A missed
// .get() is always `undefined` here — see the wrapper's null -> undefined pin.
export interface SqliteStatement<R = SqlRow> {
  run(...params: SqlValue[]): SqlRunResult;
  get(...params: SqlValue[]): R | undefined;
  all(...params: SqlValue[]): R[];
}

// The wrapped, driver-uniform handle every other module threads through `ctx`.
export interface SqliteHandle {
  exec(sql: string): void;
  prepare<R = SqlRow>(sql: string): SqliteStatement<R>;
  close(): void;
}

// The subset of each driver's own handle that wrap() actually touches. Both
// node:sqlite's DatabaseSync and bun:sqlite's Database satisfy it structurally,
// but their published types spell rows differently (node's SQLOutputValue vs
// bun's `any`), so the seam asserts this one shape at construction and reads
// every row back as `unknown` rather than threading either driver's row type
// through fleetd.
interface DriverStatement {
  run(...params: SqlValue[]): SqlRunResult;
  get(...params: SqlValue[]): unknown;
  all(...params: SqlValue[]): unknown[];
}
interface DriverHandle {
  exec(sql: string): void;
  prepare(sql: string): DriverStatement;
  close(): void;
}

let makeHandle: (file: string) => SqliteHandle;

if (process.versions.bun) {
  const { Database } = await import('bun:sqlite');
  makeHandle = (file) => wrap(new Database(file));
} else {
  // node:sqlite emits a single ExperimentalWarning the instant it is imported.
  // Intercept ONLY that one emission at its source: removing `warning` listeners
  // would clobber handlers installed by launchers, test runners and
  // observability tooling, while installing our own formatter would lose Node's
  // normal warning detail. Every pre-existing listener and every unrelated
  // warning is left alone, and the original emitWarning is restored in `finally`
  // even if the import throws.
  // eslint-disable-next-line @typescript-eslint/unbound-method -- captured verbatim so `finally` restores the exact original method object; only ever forwarded through with its receiver preserved (.call below), never invoked free-floating.
  const emitWarning = process.emitWarning;
  process.emitWarning = function fleetdSqliteWarningFilter(
    this: unknown,
    warning: string | Error,
    type?: string | { type?: string },
    ...args: unknown[]
  ): void {
    const name =
      warning instanceof Error ? warning.name : typeof type === 'string' ? type : type?.type;
    const message = warning instanceof Error ? warning.message : warning;
    if (name === 'ExperimentalWarning' && /^SQLite is an experimental feature\b/i.test(message))
      return;
    (emitWarning as unknown as (this: unknown, ...a: unknown[]) => void).call(
      this,
      warning,
      type,
      ...args,
    );
  } as unknown as typeof process.emitWarning;
  let DatabaseSync: typeof import('node:sqlite').DatabaseSync;
  try {
    ({ DatabaseSync } = await import('node:sqlite'));
  } finally {
    process.emitWarning = emitWarning;
  }
  makeHandle = (file) => wrap(new DatabaseSync(file));
}

// One thin, uniform wrapper for both drivers. It delegates 1:1 except for the
// single normalization noted above (a missed .get() -> undefined), so the object
// the rest of fleetd threads through `ctx` behaves identically on either runtime.
function wrap(handle: DriverHandle): SqliteHandle {
  return {
    exec(sql) {
      handle.exec(sql);
    },
    prepare<R = SqlRow>(sql: string): SqliteStatement<R> {
      const stmt = handle.prepare(sql);
      return {
        run: (...params) => stmt.run(...params),
        get: (...params) => {
          const row = stmt.get(...params);
          // bun:sqlite yields null for a missed row, node:sqlite yields
          // undefined; consumers read a miss with truthiness / ?? / ?., but the
          // shape is pinned to Node's so the two channels never diverge.
          return (row ?? undefined) as R | undefined;
        },
        all: (...params) => stmt.all(...params) as R[],
      };
    },
    close() {
      handle.close();
    },
  };
}

// openDatabase(file) — open a SQLite database on whichever runtime we are and
// return the wrapped handle. `file` is a filesystem path or the ':memory:'
// sentinel; both drivers accept both. This is the low-level primitive: it opens
// and wraps only. fleetd's store shape (the DDL, migrate(), and the 0600
// confidentiality chmod) lives in db.mjs's openDb(), which builds on this.
export function openDatabase(file: string): SqliteHandle {
  return makeHandle(file);
}
