// Test-only installation of P3's sole Promise-to-Effect process bridge.
//
// The source suite remains a dual-runtime trust anchor. Bun exercises the
// production Bun.spawn driver; Node exercises the extracted child_process
// reference. Both are owned by the same ManagedRuntime bridge that production
// registers with DaemonResources, so direct domain tests never acquire an
// unowned subprocess runtime of their own.
import process from 'node:process';
import * as Effect from 'effect/Effect';
import { createBootstrapProcessRuntimeBridge } from '../../src/daemon/app/bootstrap-process-runtime.ts';
import { makeProcessRunnerLayerFromDriver } from '../../src/daemon/app/services/process-driver.ts';
import { bindExecFileDelegate } from '../../src/daemon/exec.ts';
import { makeNodeProcessDriverReference } from '../../src/daemon/platform/node/process-driver-reference.ts';

const layer = process.versions.bun
  ? undefined
  : makeProcessRunnerLayerFromDriver(Effect.sync(makeNodeProcessDriverReference));
const bridge = createBootstrapProcessRuntimeBridge(layer);
const unbind = bindExecFileDelegate({ run: bridge.run, runBounded: bridge.runBounded });

let closePromise: Promise<void> | null = null;

/** Quiesce, join, and unbind the one test-process runtime. */
export function closeTestProcessRuntime(): Promise<void> {
  closePromise ??= Promise.resolve().then(async () => {
    bridge.quiesce();
    try {
      await bridge.close();
    } finally {
      unbind();
    }
  });
  return closePromise;
}
