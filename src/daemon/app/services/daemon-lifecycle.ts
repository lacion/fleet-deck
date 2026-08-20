import * as Context from 'effect/Context';
import type { LifecycleCoordinator } from '../lifecycle-coordinator.ts';
import type { AcquiredDaemonResources } from '../program.ts';

/**
 * The P1 aggregate owner paired with P4's policy coordinator.
 *
 * This is deliberately a value service rather than a runtime. The root Layer
 * owns its lifetime; callback adapters may only observe the already-acquired
 * value through the root Context.
 */
export interface DaemonLifecycleService {
  readonly acquired: AcquiredDaemonResources;
  readonly coordinator: LifecycleCoordinator;
}

export class DaemonLifecycle extends Context.Service<DaemonLifecycle, DaemonLifecycleService>()(
  'fleetdeck/daemon/app/DaemonLifecycle',
) {}
