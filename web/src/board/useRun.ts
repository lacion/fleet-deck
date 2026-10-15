import { useState } from 'react';

const message = (error: unknown) =>
  error instanceof Error ? error.message : 'The deck did not answer.';

/** Runs one deck call at a time with a busy flag and the text of its failure. */
export function useRun() {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = (work: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    void work()
      .catch((e: unknown) => setError(message(e)))
      .finally(() => setBusy(false));
  };
  return { busy, error, run };
}
