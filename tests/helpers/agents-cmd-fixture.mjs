#!/usr/bin/env node
// tests/helpers/agents-cmd-fixture.mjs
//
// Stand-in for the real `claude agents --json` CLI, used as the
// FLEETDECK_AGENTS_CMD override in tests/agents-ingest.test.mjs so the
// daemon's poller (scripts/fleetd/agents-poll.mjs) ingests deterministic
// fixture data instead of shelling out to a real (and possibly absent)
// `claude` binary.
//
// Reads the JSON file path from FLEETDECK_TEST_AGENTS_FIXTURE and prints its
// contents verbatim to stdout. Prints "[]" if the env var is unset or the
// file can't be read, so a stray/late poll never throws instead of skipping.
// Tests can rewrite the fixture file between polls to change what the next
// tick sees.

import { readFileSync } from 'node:fs';

const file = process.env.FLEETDECK_TEST_AGENTS_FIXTURE;
if (!file) {
  process.stdout.write('[]');
} else {
  try {
    process.stdout.write(readFileSync(file, 'utf8'));
  } catch {
    process.stdout.write('[]');
  }
}
