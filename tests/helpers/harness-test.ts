// harness-test.ts — the runtime-agnostic test-harness seam the suite imports its
// `test`, `after`, and `TestContext` through, mirroring sqlite.ts's driver seam.
//
// The suite must stay green on TWO runtimes across the Bun-primary migration:
// `node --test` (the authoritative trust anchor) and `bun test` (the target
// single runtime). node:test and bun:test are NOT interchangeable. bun ships a
// node:test compat shim that cannot sequence multiple async top-level test()
// calls — it runs each eagerly, so the next registration lands "inside" the
// previous, still-awaiting one (ERR_NOT_IMPLEMENTED, oven-sh/bun#5090) — and it
// passes the body no per-test `t` context. This module is the single guarded
// seam: it picks the backend once, at import, off process.versions.bun. Under
// Node it is a pure passthrough to node:test, so the trust anchor stays
// byte-for-byte unchanged; under Bun it reconstructs the slice of node:test's
// surface the suite uses on top of bun:test's native registrar (which sequences
// top-level tests correctly): the overloaded (name, options?, fn) signature, the
// per-test `t` context's .after / .diagnostic / .skip / .test, the { skip, todo,
// only, timeout } options, and the top-level hooks. Every test file imports
// `test` (and `after` / `TestContext`) from here, never from node:test, so no
// file names either runner's builtin — exactly the sqlite.ts contract.

import process from 'node:process';
import type { TestContext } from 'node:test';
import { closeTestProcessIngress } from './ingress-process-facade-test.ts';

export type { TestContext };

type TestBody = (t: TestContext) => unknown;
type Hook = () => unknown;

interface TestOptions {
  skip?: boolean | string;
  todo?: boolean | string;
  only?: boolean;
  timeout?: number;
  concurrency?: boolean | number;
}

interface TestApi {
  (name: string, body: TestBody): void;
  (name: string, options: TestOptions, body: TestBody): void;
  skip: (name: string, body?: TestBody) => void;
  only: (name: string, body: TestBody) => void;
  todo: (name: string, body?: TestBody) => void;
}

let test: TestApi;
let after: (fn: Hook) => void;
let before: (fn: Hook) => void;
let beforeEach: (fn: Hook) => void;
let afterEach: (fn: Hook) => void;
let describe: (name: string, fn: () => void) => void;

if (process.versions.bun) {
  const bt = await import('bun:test');

  // node:test's default per-test timeout is Infinity; bun's is 5000ms. Mirror
  // node's effectively-unbounded default so a legitimately slow test (e.g. an
  // mDNS probing window) is not cut at 5s and reported as a false failure. An
  // explicit `{ timeout }` option still wins. A genuine hang is capped here so
  // it fails the run instead of wedging it.
  const DEFAULT_TIMEOUT_MS = 60_000;

  // Runtime `t.skip('reason')` (always followed by `return` in this suite)
  // throws this sentinel; invoke() swallows it so the test short-circuits to a
  // pass. Bun has no "skip the current test from within", so pass is the
  // faithful mapping for the gate — the env-dependent assertions never run.
  const SKIP = Symbol('harness.skip');

  // node:test's test()/t.test() are overloaded: (name?, options?, fn?). Pull the
  // three roles out by type regardless of arity or order.
  const parse = (
    args: unknown[],
  ): { name: string; options: TestOptions; body?: TestBody | undefined } => {
    let name = '';
    let options: TestOptions = {};
    let body: TestBody | undefined;
    for (const arg of args) {
      if (typeof arg === 'string') name = arg;
      else if (typeof arg === 'function') body = arg as TestBody;
      else if (arg && typeof arg === 'object') options = arg;
    }
    return { name, options, body };
  };

  interface Outcome {
    threw: boolean;
    err: unknown;
    subFailures: { name: string; err: unknown }[];
  }

  // Run one body with a freshly-built node:test-shaped `t`. Recurses for
  // subtests (bun forbids a real nested test()), so nesting, per-context LIFO
  // afters, and runtime t.skip() all behave as under node. A subtest's failure
  // is captured rather than rethrown at once, so sibling subtests still run
  // (node isolates subtests) and the parent fails iff ANY subtest did.
  const invoke = async (body: TestBody | undefined): Promise<Outcome> => {
    const afters: Hook[] = [];
    const subFailures: { name: string; err: unknown }[] = [];
    const ctx = {
      after(fn: Hook) {
        afters.push(fn);
      },
      diagnostic(message: string) {
        process.stderr.write(`# ${message}\n`);
      },
      skip(_message?: string): void {
        throw SKIP;
      },
      todo(_message?: string): void {
        throw SKIP;
      },
      async test(...subArgs: unknown[]): Promise<void> {
        const { name, options, body: sub } = parse(subArgs);
        if (options.skip || options.todo || !sub) return;
        const out = await invoke(sub);
        if (out.threw) subFailures.push({ name, err: out.err });
        else
          for (const f of out.subFailures)
            subFailures.push({ name: `${name} > ${f.name}`, err: f.err });
      },
      get signal(): AbortSignal {
        return new AbortController().signal;
      },
      get name(): string {
        return '';
      },
    };
    let threw = false;
    let err: unknown;
    try {
      if (body) await body(ctx as unknown as TestContext);
    } catch (e) {
      if (e !== SKIP) {
        threw = true;
        err = e;
      }
    } finally {
      // node:test runs a test's t.after hooks in REGISTRATION (FIFO) order —
      // verified against `node --test`. A test that registers a cleanup hook
      // and THEN a hook asserting the cleanup happened (the gateway drain
      // probes) relies on the cleanup running first, so this must not reverse.
      for (const fn of afters) await fn();
    }
    return { threw, err, subFailures };
  };

  const runBody = (body: TestBody | undefined) => async (): Promise<void> => {
    const out = await invoke(body);
    if (out.threw) throw out.err;
    if (out.subFailures.length === 1) {
      const only = out.subFailures[0];
      if (only) throw only.err;
    }
    if (out.subFailures.length > 1)
      throw new AggregateError(
        out.subFailures.map((f) => f.err),
        `subtests failed: ${out.subFailures.map((f) => f.name).join(', ')}`,
      );
  };

  const bunTest = bt.test;

  // A test module imported OUTSIDE `bun test` — the wait-scaling probe imports
  // spawn-repo.test.ts as a plain script purely to read one exported binding —
  // makes bun:test's registrar throw "Cannot use test outside of the test
  // runner". That import wants the module's EXPORTS, not its tests, so a
  // registration attempted outside the runner is a no-op here instead of a
  // crash. Under a real `bun test` run the registrar never throws, so this
  // changes nothing; a throw with any other message still propagates.
  const OUTSIDE_RUNNER = 'outside of the test runner';
  const register = (fn: () => void): void => {
    try {
      fn();
    } catch (e) {
      if (e instanceof Error && e.message.includes(OUTSIDE_RUNNER)) return;
      throw e;
    }
  };

  const api = ((...args: unknown[]): void => {
    const { name, options, body } = parse(args);
    const timeout = options.timeout ?? DEFAULT_TIMEOUT_MS;
    if (options.skip) {
      register(() => {
        bunTest.skip(name, () => {}, timeout);
      });
      return;
    }
    if (options.todo) {
      register(() => {
        bunTest.todo(name, body ? runBody(body) : () => {}, timeout);
      });
      return;
    }
    if (options.only) {
      register(() => {
        bunTest.only(name, runBody(body), timeout);
      });
      return;
    }
    register(() => {
      bunTest(name, runBody(body), timeout);
    });
  }) as TestApi;
  api.skip = (name, _body) => {
    register(() => {
      bunTest.skip(name, () => {}, DEFAULT_TIMEOUT_MS);
    });
  };
  api.only = (name, body) => {
    register(() => {
      bunTest.only(name, runBody(body), DEFAULT_TIMEOUT_MS);
    });
  };
  api.todo = (name, body) => {
    register(() => {
      bunTest.todo(name, body ? runBody(body) : () => {}, DEFAULT_TIMEOUT_MS);
    });
  };
  test = api;
  after = (fn) => {
    register(() => {
      bt.afterAll(fn);
    });
  };
  before = (fn) => {
    bt.beforeAll(fn);
  };
  beforeEach = (fn) => {
    bt.beforeEach(fn);
  };
  afterEach = (fn) => {
    bt.afterEach(fn);
  };
  describe = (name, fn) => {
    bt.describe(name, fn);
  };
} else {
  const nt = await import('node:test');
  test = (nt.default ?? nt.test) as unknown as TestApi;
  after = nt.after;
  before = nt.before;
  beforeEach = nt.beforeEach;
  afterEach = nt.afterEach;
  describe = nt.describe;
}

// Direct source tests call domain modules without booting fleetd. Install the
// same root-owned P4 ingress facade once per test process. node:test isolates files in
// workers, so its global hook owns that worker's runtime. Bun shares this module
// instance across files but scopes afterAll to whichever file imported it first;
// closing there would unbind later files. Its process-level beforeExit event is
// therefore the only suite-wide owner and runs once the last file is idle.
if (process.versions.bun) {
  process.once('beforeExit', () => closeTestProcessIngress());
} else {
  after(closeTestProcessIngress);
}

export { test as default, test, after, before, beforeEach, afterEach, describe };
