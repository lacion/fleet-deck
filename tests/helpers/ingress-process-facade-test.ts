// Test-only installation of P4's root-owned Promise-to-Effect process facade.
//
// The source suite remains a dual-runtime trust anchor. Bun exercises the
// production Bun.spawn driver; Node exercises the extracted child_process
// reference. One explicit test Scope owns the selected ProcessRunner Layer,
// and one IngressSupervisor captures that already-built Context. The facade
// owns neither a runtime nor resources of its own.
import process from 'node:process';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Layer from 'effect/Layer';
import * as Scope from 'effect/Scope';
import { makeIngressExecFileDelegate } from '../../src/daemon/app/legacy-process-facade.ts';
import { makeProcessRunnerLayerFromDriver } from '../../src/daemon/app/services/process-driver.ts';
import { bindExecFileDelegate } from '../../src/daemon/exec.ts';
import { makeIngressSupervisor } from '../../src/daemon/platform/bun/ingress-supervisor-live.ts';
import { ProcessRunnerLive } from '../../src/daemon/platform/bun/process-runner-live.ts';
import { makeNodeProcessDriverReference } from '../../src/daemon/platform/node/process-driver-reference.ts';

const layer = process.versions.bun
  ? ProcessRunnerLive
  : makeProcessRunnerLayerFromDriver(Effect.sync(makeNodeProcessDriverReference));
const rootScope = Scope.makeUnsafe('sequential');
const runTestPromise = Effect.runPromiseWith(Context.empty());
const { ingress, unbind } = await (async () => {
  try {
    const context = await runTestPromise(Layer.buildWithScope(layer, rootScope));
    const ingress = await runTestPromise(makeIngressSupervisor(context, rootScope));
    const unbind = bindExecFileDelegate(makeIngressExecFileDelegate(ingress));
    return { ingress, unbind };
  } catch (error) {
    await runTestPromise(Scope.close(rootScope, Exit.void));
    throw error;
  }
})();

let closePromise: Promise<void> | null = null;

/** Quiesce ingress, join its fibers, release the driver Scope, and unbind. */
export function closeTestProcessIngress(): Promise<void> {
  closePromise ??= Promise.resolve().then(async () => {
    ingress.quiesce();
    try {
      await ingress.close();
    } finally {
      try {
        await runTestPromise(Scope.close(rootScope, Exit.void));
      } finally {
        unbind();
      }
    }
  });
  return closePromise;
}
