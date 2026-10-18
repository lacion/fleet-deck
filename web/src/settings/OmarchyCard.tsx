// Omarchy admin task D (design `docs/v2/design/omarchy-admin.md` §5): the Omarchy card on the
// station tab of Settings. It shows the state of Omarchy on the station and runs the actions
// (install, uninstall, the screensaver, retry provisioning). Each action asks the elevation first.
import { useCallback, useEffect, useRef, useState } from 'react';
import { Icon, RefreshCw } from '../ui/icons';
import type { ElevationAsk } from './elevation';
import {
  buttons,
  type CardState,
  cardLines,
  LOADING_TEXT,
  type OmarchyAction,
  type OmarchyActions,
  type OmarchyStatus,
  pollRetry,
  READ_ERROR_TEXT,
  runAction,
  stateOfReadError,
  UNSUPPORTED_TEXT,
  WORKING_TEXT,
} from './omarchy';

/** A failed action: its fixed text, and the message of the station as plain text. */
export type Failure = { readonly text: string; readonly message: string | null };

type ViewProps = {
  readonly state: CardState;
  readonly busy: boolean;
  readonly failure: Failure | null;
  readonly onAct: (action: OmarchyAction) => void;
  readonly onRetryRead: () => void;
};

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/** The card for a state. No card for an older deck (404). */
export function OmarchyView({ state, busy, failure, onAct, onRetryRead }: ViewProps) {
  if (state.kind === 'hidden') return null;
  let body: React.ReactNode;
  if (state.kind === 'loading') {
    body = <p role="status">{LOADING_TEXT}</p>;
  } else if (state.kind === 'unsupported') {
    body = <p className="fd-note">{UNSUPPORTED_TEXT}</p>;
  } else if (state.kind === 'error') {
    body = (
      <div className="fd-set-notice" data-tone="error" role="alert">
        <span>{READ_ERROR_TEXT}</span>
        <button type="button" className="fd-sm" onClick={onRetryRead}>
          <Icon of={RefreshCw} size={14} /> Retry
        </button>
      </div>
    );
  } else {
    const lines = cardLines(state.status);
    const shown = buttons(state.status);
    const working = busy || state.status.running !== null;
    body = (
      <>
        {lines.map((line) => (
          <p className="fd-note" key={line}>
            {line}
          </p>
        ))}
        {busy && state.status.running === null ? <p role="status">{WORKING_TEXT}</p> : null}
        {failure === null ? null : (
          <div className="fd-set-notice" data-tone="error" role="alert">
            <span>{failure.text}</span>
            {failure.message === null ? null : <pre>{failure.message}</pre>}
          </div>
        )}
        {shown.length === 0 ? null : (
          <div className="fd-set-bar-actions">
            {shown.map((b) => (
              <button
                key={b.action}
                type="button"
                className="fd-sm"
                disabled={working}
                onClick={() => onAct(b.action)}
              >
                {b.label}
              </button>
            ))}
          </div>
        )}
      </>
    );
  }
  return (
    <section className="fd-panel fd-omarchy" aria-label="Omarchy">
      <h2>Omarchy</h2>
      {body}
    </section>
  );
}

type Props = {
  /** The station name, for the path of the calls. */
  readonly station: string;
  readonly actions: OmarchyActions;
  readonly ask: ElevationAsk;
  /** The state at the first render (tests). */
  readonly initial?: CardState;
};

export function OmarchyCard({ station, actions, ask, initial }: Props) {
  const [state, setState] = useState<CardState>(initial ?? { kind: 'loading' });
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<Failure | null>(null);
  const turn = useRef(0);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const load = useCallback(() => {
    const mine = ++turn.current;
    setState({ kind: 'loading' });
    actions.status(station).then(
      (status) => {
        if (mine === turn.current) setState({ kind: 'ready', status });
      },
      (e: unknown) => {
        if (mine === turn.current) setState(stateOfReadError(e));
      },
    );
  }, [actions, station]);
  useEffect(() => {
    if (initial === undefined) load();
    return () => {
      turn.current += 1;
    };
  }, [load, initial]);

  const show = (status: OmarchyStatus) => setState({ kind: 'ready', status });

  const act = async (action: OmarchyAction) => {
    setFailure(null);
    setBusy(true);
    const mine = ++turn.current;
    const end = await runAction(actions, station, action, ask);
    if (!alive.current) return;
    setBusy(false);
    if (end.kind === 'cancelled') return;
    if (end.kind === 'failed') {
      if (end.status !== null) show(end.status);
      setFailure({ text: end.text, message: end.message });
      return;
    }
    show(end.status);
    if (end.kind === 'started') {
      await pollRetry(actions, station, {
        sleep,
        now: Date.now,
        stopped: () => !alive.current || mine !== turn.current,
        onStatus: show,
      });
    }
  };

  return (
    <OmarchyView
      state={state}
      busy={busy}
      failure={failure}
      onAct={(action) => void act(action)}
      onRetryRead={load}
    />
  );
}
