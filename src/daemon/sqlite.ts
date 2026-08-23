// sqlite.ts — the SQLite handle fleetd opens its store through.
//
// fleetd is Bun-only (since 0.23.0 bin/fleetdeck.ts's serve() preflight exits
// EX_CONFIG on any non-Bun runtime), so the store has exactly one driver:
// bun:sqlite's Database. This module is the single seam that names it — every
// other file opens through openDatabase(), so no other module imports the
// builtin. (Historically this seam also carried a node:sqlite arm for the Node
// plugin path, picked at import off process.versions.bun; that path is retired,
// so the seam is now a plain static import of bun:sqlite.)
//
// fleetd uses a tiny slice of the driver: positional `?` binding, multi-statement
// .exec(), plain-object rows, and a .run() result carrying { changes,
// lastInsertRowid } as plain numbers. The one quirk the wrapper normalizes is a
// missed .get(): bun:sqlite returns `null`, and the wrapper below pins that to
// `undefined` so consumers reading a miss with truthiness / ?? / ?. see a stable
// sentinel (the historical node:sqlite parity, kept so the store's behavior did
// not shift when the runtime unified). openDatabase() returns ONE stable object
// per open — statements.ts keys a WeakMap on the handle to cache prepared
// statements, so the handle identity must stay durable for the life of the
// connection.

import { Database } from 'bun:sqlite';

// The store's foundational value types. Every row shape db.ts and statements.ts
// declare is built on top of these: a cell is one of SQLite's storage classes, a
// row is a column-keyed record, and a caller asserts the concrete row shape via
// the `R` parameter of prepare() — the SQL text, not the driver, is what
// guarantees that shape, so the assertion belongs with each query, not here.
export type SqlValue = null | number | bigint | string | Uint8Array;
export type SqlRow = Record<string, SqlValue>;

// What .run() reports. bun:sqlite carries these as number | bigint (a rowid or
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
  // Release this one statement's compiled sqlite3_stmt (sqlite3_finalize),
  // idempotent. The only correct way to free an EPHEMERAL prepare: a one-shot
  // `db.prepare(sql).run(...)` site must finalize() after its single use so the
  // handle's statement set does not retain it for the connection's whole life.
  // Every other prepare in fleetd is compiled once and lives for the handle
  // (statements.ts's `q`, questions.ts's `q`), freed collectively by the
  // handle's finalizeStatements() at close.
  finalize(): void;
}

// The wrapped, driver-uniform handle every other module threads through `ctx`.
export interface SqliteHandle {
  exec(sql: string): void;
  prepare<R = SqlRow>(sql: string): SqliteStatement<R>;
  // Finalize every prepared statement this handle still owns (sqlite3_finalize
  // each), then forget them. The store owner calls this immediately before
  // close(true) so sqlite3_close cannot fail on — or defer the real close
  // behind — a still-live statement. Idempotent: a second call finalizes the
  // now-empty set.
  finalizeStatements(): void;
  // Close the underlying connection. `throwOnError` selects bun:sqlite's close
  // mode: the default (false / omitted) is sqlite3_close_v2, which DEFERS the
  // actual close behind any still-open statement; `true` is sqlite3_close,
  // which closes IMMEDIATELY and throws if a statement is still live. Every
  // legacy caller passes no argument (unchanged sqlite3_close_v2 behavior);
  // only the store owner passes true, after finalizeStatements() has freed the
  // set (P8.4: the close then completes in the finalizer, not deferred past it).
  close(throwOnError?: boolean): void;
}

// The subset of bun:sqlite's Database/Statement that wrap() actually touches.
// bun's published row type is `any`, so the seam asserts this one narrow shape at
// construction and reads every row back as `unknown` rather than letting bun's
// `any` thread through fleetd.
interface DriverStatement {
  run(...params: SqlValue[]): SqlRunResult;
  get(...params: SqlValue[]): unknown;
  all(...params: SqlValue[]): unknown[];
  // bun:sqlite's Statement.finalize() — sqlite3_finalize, idempotent.
  finalize(): void;
}
interface DriverHandle {
  exec(sql: string): void;
  prepare(sql: string): DriverStatement;
  // bun:sqlite's Database.close(throwOnError?) — the arg selects
  // sqlite3_close_v2 (default) vs sqlite3_close (true). See SqliteHandle.close.
  close(throwOnError?: boolean): void;
}

// fleetd's single SQLite driver is bun:sqlite's Database. makeHandle opens one
// and wraps it into the uniform handle below. (wrap is a hoisted function
// declaration, so the forward reference here is fine.)
function makeHandle(file: string): SqliteHandle {
  return wrap(new Database(file));
}

// One thin wrapper over the bun:sqlite handle. It delegates 1:1 except for the
// single normalization noted above (a missed .get() -> undefined), so the object
// the rest of fleetd threads through `ctx` presents a stable, driver-independent
// surface.
function wrap(handle: DriverHandle): SqliteHandle {
  // Every prepared statement this handle hands out, tracked so the store owner
  // can finalize the whole set before an immediate close(true). Compile-once
  // statements (statements.ts's `q`, questions.ts's `q`) live here for the
  // connection's life; an ephemeral one-shot removes itself via its finalize().
  const statements = new Set<DriverStatement>();
  return {
    exec(sql) {
      handle.exec(sql);
    },
    prepare<R = SqlRow>(sql: string): SqliteStatement<R> {
      const stmt = handle.prepare(sql);
      statements.add(stmt);
      return {
        run: (...params) => stmt.run(...params),
        get: (...params) => {
          const row = stmt.get(...params);
          // bun:sqlite yields null for a missed row; pin it to undefined so a
          // miss read with truthiness / ?? / ?. sees a stable sentinel (the
          // historical node:sqlite parity, kept unchanged as the runtime unified).
          return (row ?? undefined) as R | undefined;
        },
        all: (...params) => stmt.all(...params) as R[],
        finalize: () => {
          stmt.finalize();
          statements.delete(stmt);
        },
      };
    },
    finalizeStatements() {
      for (const stmt of statements) stmt.finalize();
      statements.clear();
    },
    close(throwOnError) {
      handle.close(throwOnError);
    },
  };
}

// openDatabase(file) — open a SQLite database via bun:sqlite and return the
// wrapped handle. `file` is a filesystem path or the ':memory:' sentinel;
// bun:sqlite accepts both. This is the low-level primitive: it opens and wraps
// only. fleetd's store shape (the DDL, migrate(), and the 0600 confidentiality
// chmod) lives in db.ts's openDb(), which builds on this.
export function openDatabase(file: string): SqliteHandle {
  return makeHandle(file);
}
