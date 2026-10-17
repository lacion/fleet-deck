// DL-172: "Confirm it is you" as a promise. `ask` answers at once with an elevation that is still
// valid; else it opens the dialog and answers with the new elevation, or `null` when the owner
// cancels. The elevation lives in the actions (memory only).
import { type ReactNode, useCallback, useEffect, useRef, useState } from 'react';
import type { SettingsActions } from './actions';
import { ElevationDialog } from './dialogs';
import { elevate } from './flow';

type Request = {
  readonly labels: readonly string[];
  readonly resolve: (token: string | null) => void;
};

export type ElevationAsk = (labels?: readonly string[]) => Promise<string | null>;

export function useElevation(actions: SettingsActions): {
  readonly ask: ElevationAsk;
  readonly node: ReactNode;
} {
  const [request, setRequest] = useState<Request | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const open = useRef<Request | null>(null);
  open.current = request;
  // A dialog that is open when the page goes away answers "cancelled".
  useEffect(() => () => open.current?.resolve(null), []);
  const ask = useCallback<ElevationAsk>(
    (labels = []) => {
      const held = actions.elevation();
      if (held !== null) return Promise.resolve(held);
      return new Promise<string | null>((resolve) => {
        open.current?.resolve(null);
        setError(null);
        setBusy(false);
        setRequest({ labels, resolve });
      });
    },
    [actions],
  );
  const finish = (token: string | null) => {
    request?.resolve(token);
    setRequest(null);
  };
  const node =
    request === null ? null : (
      <ElevationDialog
        labels={request.labels}
        busy={busy}
        error={error}
        onCancel={() => finish(null)}
        onSubmit={(credentials) => {
          setBusy(true);
          setError(null);
          void elevate(actions, credentials).then((outcome) => {
            setBusy(false);
            if (outcome.kind === 'ok') finish(actions.elevation());
            else setError(outcome.text);
          });
        }}
      />
    );
  return { ask, node };
}
