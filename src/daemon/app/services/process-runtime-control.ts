import * as Context from 'effect/Context';

/**
 * Root-only ownership surface for the one ProcessRunner driver instance.
 * `force` is safe to call from a signal/deadline callback and starts an
 * immediate SIGKILL + joined close; `close` returns that exact same completion.
 */
export interface ProcessRuntimeControlService {
  readonly force: () => void;
  readonly close: () => Promise<void>;
}

export class ProcessRuntimeControl extends Context.Service<
  ProcessRuntimeControl,
  ProcessRuntimeControlService
>()('fleetdeck/daemon/app/ProcessRuntimeControl') {}
