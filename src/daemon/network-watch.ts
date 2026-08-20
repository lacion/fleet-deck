export interface NetworkWatchOwner {
  stop: () => Promise<void>;
}

export interface NetworkWatchOptions {
  enabled?: boolean;
  intervalMs: number;
  onChange: (addresses: string[], previous: string[]) => void | Promise<void>;
  onError?: (error: unknown) => void;
  previousAddresses: () => readonly string[] | null;
  readAddresses: () => readonly string[] | null;
}

export function sameAddressSet(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && [...a].sort().join(' ') === [...b].sort().join(' ');
}

/**
 * Own the LAN refresh timer and any callback currently running from it.
 * stop() is idempotent, prevents new callbacks, and joins the current callback.
 */
export function startNetworkWatch({
  enabled = true,
  intervalMs,
  onChange,
  onError = () => {
    /* best-effort watcher */
  },
  previousAddresses,
  readAddresses,
}: NetworkWatchOptions): NetworkWatchOwner {
  let stopped = false;
  let timer: ReturnType<typeof setInterval> | null = null;
  let currentTick: Promise<void> | null = null;
  let stopPromise: Promise<void> | null = null;

  async function tick(): Promise<void> {
    let addresses: readonly string[] | null = null;
    try {
      addresses = readAddresses();
    } catch {
      return;
    }
    if (stopped || addresses === null) return;
    const previous = previousAddresses();
    if (stopped || previous === null || sameAddressSet(addresses, previous)) return;
    await onChange([...addresses], [...previous]);
  }

  function launchTick(): void {
    if (stopped || currentTick) return;
    // Register the Promise before tick() executes so stop() always sees a tick
    // even when its first callback synchronously initiates shutdown.
    const active = Promise.resolve()
      .then(tick)
      .catch((error: unknown) => {
        try {
          onError(error);
        } catch {
          /* watcher errors stay non-load-bearing */
        }
      });
    currentTick = active;
    void active.then(() => {
      if (currentTick === active) currentTick = null;
    });
  }

  if (enabled) {
    timer = setInterval(launchTick, intervalMs);
    // Deliberately referenced: lifecycle shutdown owns stop() and must clear
    // the cadence before the process is allowed to exit naturally.
  }

  return {
    stop(): Promise<void> {
      if (stopPromise) return stopPromise;
      stopped = true;
      if (timer) {
        clearInterval(timer);
        timer = null;
      }
      const active = currentTick;
      stopPromise = active ? active.then(() => undefined) : Promise.resolve();
      return stopPromise;
    },
  };
}
