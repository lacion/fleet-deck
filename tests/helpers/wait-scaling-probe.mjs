// Subprocess probe for tests/wait-scaling.test.mjs (BUG-176). NOT a test file
// (no .test.mjs suffix — the runner must never pick it up directly).
//
// Invoked as: node --test helpers/wait-scaling-probe.mjs, with the target test
// file in FLEETDECK_PROBE_TARGET (node's test runner consumes positional args
// as test globs, so the target rides an env var).
//
// Imports the target test module (registering its tests), then — as the last
// statement of the module, with no further awaits, so the runner has not yet
// STARTED any test body — asserts by identity that the target's exported
// waitUntil IS the shared scaled helper from tests/helpers/wait.mjs, with
// FLEETDECK_TEST_WAIT_SCALE active in this process. Verdict is one JSON line
// on stdout (under `node --test`, child-process stderr is folded into the TAP
// comment stream, so stdout is the reliable channel) and process.exit is
// immediate and clean.

import path from 'node:path';
import { pathToFileURL } from 'node:url';

const target = process.env.FLEETDECK_PROBE_TARGET;
const here = path.dirname(new URL(import.meta.url).pathname);

const mod = await import(pathToFileURL(target).href);
const { waitUntil: shared } = await import(pathToFileURL(path.join(here, 'wait.mjs')).href);
const exported = mod.__waitUntilForScaleCheck;
const ok = exported === shared;
process.stdout.write(`PROBE ${JSON.stringify({ exported: typeof exported, ok })}\n`);
process.exit(ok ? 0 : 1);
