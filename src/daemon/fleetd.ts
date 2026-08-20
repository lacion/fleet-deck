#!/usr/bin/env bun
// One host entry, one Effect root. All construction/finalization stays inside
// DaemonApp's provided Scope; this file only selects Bun's process runner.

import * as BunRuntime from '@effect/platform-bun/BunRuntime';
import { DaemonApp, daemonHostControl } from './app/root-program.ts';

BunRuntime.runMain(DaemonApp, {
  disableErrorReporting: true,
  teardown: daemonHostControl.teardown,
});
