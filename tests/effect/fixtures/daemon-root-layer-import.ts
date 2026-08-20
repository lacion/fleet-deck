import {
  composeDaemonRootLayer,
  makeDaemonLifecycleLayer,
} from '../../../src/daemon/app/live-layer.ts';
import { DaemonLifecycle } from '../../../src/daemon/app/services/daemon-lifecycle.ts';

if (
  typeof composeDaemonRootLayer !== 'function' ||
  typeof makeDaemonLifecycleLayer !== 'function' ||
  typeof DaemonLifecycle.key !== 'string'
) {
  throw new Error('daemon root Layer definitions did not load');
}

process.stdout.write('definition-only\n');
