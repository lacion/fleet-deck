// sqlite.mjs — the runtime-agnostic SQLite handle fleetd opens its store through.
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

let makeHandle;

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
  const emitWarning = process.emitWarning;
  process.emitWarning = function fleetdSqliteWarningFilter(warning, type, ...args) {
    const name = warning instanceof Error
      ? warning.name
      : (typeof type === 'string' ? type : type?.type);
    const message = warning instanceof Error ? warning.message : String(warning);
    if (name === 'ExperimentalWarning' && /^SQLite is an experimental feature\b/i.test(message)) return;
    return emitWarning.call(this, warning, type, ...args);
  };
  let DatabaseSync;
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
function wrap(handle) {
  return {
    exec(sql) {
      handle.exec(sql);
    },
    prepare(sql) {
      const stmt = handle.prepare(sql);
      return {
        run: (...params) => stmt.run(...params),
        get: (...params) => {
          const row = stmt.get(...params);
          // bun:sqlite yields null for a missed row, node:sqlite yields
          // undefined; consumers read a miss with truthiness / ?? / ?., but the
          // shape is pinned to Node's so the two channels never diverge.
          return row == null ? undefined : row;
        },
        all: (...params) => stmt.all(...params),
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
export function openDatabase(file) {
  return makeHandle(file);
}
