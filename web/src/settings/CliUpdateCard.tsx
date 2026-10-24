// B28 (design `docs/v2/design/cli-update.md` §3.12): the "Update now" card on the station tab of
// Settings. One row for each provider: its install, the last attempt, and the buttons `Check`
// and `Update now…`. An update asks the elevation, then a dialog; the card polls while it runs.
import { useCallback, useEffect, useRef, useState } from 'react';
import { UpdateLine } from '../board/StationCliUpdate';
import type { CliUpdateReport, ProviderKind, ProviderUpdateState } from '../gen';
import { Dialog } from '../integrations/Dialog';
import { ErrorNotice } from '../ui/page';
import { Loading } from '../ui/Loading';
import { CardRow } from './CardRow';
import {
  askUpdate,
  type BusyView,
  type CliUpdateActions,
  dialogText,
  dialogTitle,
  LOADING_TEXT,
  pollUntilIdle,
  providerLine,
  providerName,
  READ_ERROR_TEXT,
  reasonText,
  rowTitle,
  runCheck,
  runningText,
  runUpdate,
  UNSUPPORTED_TEXT,
} from './cliUpdate';
import type { ElevationAsk } from './elevation';
import { DeckError } from './http';
import { sectionId } from './model';
import { NoticeIcon } from './NoticeIcon';
import { guarded } from './OmarchyCard';

export type ReportState =
  | { readonly kind: 'loading' }
  | { readonly kind: 'unsupported' }
  | { readonly kind: 'error' }
  | { readonly kind: 'ready'; readonly report: CliUpdateReport };

/** A note under a row: the result of `Check` (`info`), or a failure (`error`). */
export type Note = { readonly text: string; readonly tone: 'info' | 'error' };

/** The open dialog: the provider, the state that it names, the elevation and the busy answer. */
export type DialogState = {
  readonly state: ProviderUpdateState;
  readonly elevation: string;
  readonly busy: boolean;
  readonly sessions: BusyView | null;
  readonly force: boolean;
};

export type Pins = { readonly claude: string | null; readonly codex: string | null };

const pinOf = (pins: Pins, provider: ProviderKind): string | null =>
  provider === 'claude' ? pins.claude : provider === 'codex' ? pins.codex : null;

/** The confirmation dialog of an update. */
export function UpdateDialog({
  station,
  dialog,
  onForce,
  onUpdate,
  onClose,
}: {
  readonly station: string;
  readonly dialog: DialogState;
  readonly onForce: (force: boolean) => void;
  readonly onUpdate: () => void;
  readonly onClose: () => void;
}) {
  const { state, sessions } = dialog;
  const stopped = sessions !== null && !sessions.canForce;
  return (
    <Dialog title={dialogTitle(state.provider, station)} onClose={onClose}>
      <div className="fd-set-dialogform">
        <p>{dialogText(state)}</p>
        {sessions === null ? null : <p role="status">{sessions.text}</p>}
        {sessions?.canForce === true ? (
          <label className="fd-checkrow">
            <input
              type="checkbox"
              checked={dialog.force}
              onChange={(e) => onForce(e.currentTarget.checked)}
            />
            <span>Update while they run</span>
          </label>
        ) : null}
        <div className="fd-formactions">
          <button type="button" className="fd-ghost" onClick={onClose}>
            Cancel
          </button>
          {stopped ? null : (
            <button type="button" className="fd-primary" disabled={dialog.busy} onClick={onUpdate}>
              Update
            </button>
          )}
        </div>
      </div>
    </Dialog>
  );
}

type ViewProps = {
  readonly station: string;
  readonly state: ReportState;
  readonly pins: Pins;
  readonly now: number;
  /** The notes of the rows, by provider. */
  readonly notes: Readonly<Record<string, Note | undefined>>;
  /** A call runs: the buttons are off. */
  readonly busy: boolean;
  readonly dialog: DialogState | null;
  readonly onCheck: (state: ProviderUpdateState) => void;
  readonly onUpdate: (state: ProviderUpdateState) => void;
  readonly onRetry: () => void;
  readonly onForce: (force: boolean) => void;
  readonly onConfirm: () => void;
  readonly onClose: () => void;
};

/** The card for a state. */
export function CliUpdateView(props: ViewProps) {
  const { state, busy } = props;
  let body: React.ReactNode;
  if (state.kind === 'loading') {
    body = <Loading text={LOADING_TEXT} />;
  } else if (state.kind === 'unsupported') {
    body = <p className="fd-note">{UNSUPPORTED_TEXT}</p>;
  } else if (state.kind === 'error') {
    body = <ErrorNotice text={READ_ERROR_TEXT} onRetry={props.onRetry} />;
  } else {
    const { report } = state;
    body = report.providers.map((p) => {
      const running = report.running !== null && p.last?.attempt_id === report.running;
      const line = providerLine(p, report.running, props.now);
      const note = props.notes[p.provider];
      const hand = p.blocked === 'report_only_method' || p.blocked === 'custom_launcher';
      return (
        <CardRow key={p.provider} label={providerName(p.provider)} className="fd-cli-update-row">
          <p>{rowTitle(p, pinOf(props.pins, p.provider))}</p>
          {running ? (
            <p role="status">
              {runningText(
                p.provider,
                p.last?.target_version ?? p.available ?? 'the newest version',
              )}
            </p>
          ) : line === null ? null : (
            <UpdateLine line={line} />
          )}
          {note === undefined ? null : note.tone === 'error' ? (
            <div className="fd-set-notice" data-tone="error" role="alert">
              <NoticeIcon tone="error" />
              <span>{note.text}</span>
            </div>
          ) : (
            <p className="fd-note" role="status">
              {note.text}
            </p>
          )}
          <div className="fd-set-bar-actions">
            <button
              type="button"
              className="fd-sm"
              disabled={busy || report.running !== null}
              onClick={() => props.onCheck(p)}
            >
              Check
            </button>
            {p.updatable ? (
              <button
                type="button"
                className="fd-sm"
                disabled={busy || report.running !== null}
                onClick={() => props.onUpdate(p)}
              >
                Update now…
              </button>
            ) : null}
          </div>
          {!p.updatable && !hand && p.blocked !== null && p.last !== null ? (
            <p className="fd-note">Not updated: {reasonText(p.blocked)}.</p>
          ) : null}
        </CardRow>
      );
    });
  }
  return (
    <section
      id={sectionId('Update now')}
      className="fd-set-sec fd-cli-update"
      aria-label="Update now"
    >
      <h2>Update now</h2>
      {body}
      {props.dialog === null ? null : (
        <UpdateDialog
          station={props.station}
          dialog={props.dialog}
          onForce={props.onForce}
          onUpdate={props.onConfirm}
          onClose={props.onClose}
        />
      )}
    </section>
  );
}

type Props = {
  /** The station name, for the path of the calls. */
  readonly station: string;
  readonly actions: CliUpdateActions;
  readonly ask: ElevationAsk;
  /** The values of the station settings, for the pins. */
  readonly values: Readonly<Record<string, unknown>>;
};

const textOf = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);

const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

/** The report with one provider's state and the attempt that runs. */
const withState = (
  report: CliUpdateReport,
  state: ProviderUpdateState,
  running: string | null,
): CliUpdateReport => ({
  ...report,
  running,
  providers: report.providers.map((p) => (p.provider === state.provider ? state : p)),
});

export function CliUpdateCard({ station, actions, ask, values }: Props) {
  const pins: Pins = {
    claude: textOf(values['provider_updates.claude_pin']),
    codex: textOf(values['provider_updates.codex_pin']),
  };
  const [state, setState] = useState<ReportState>({ kind: 'loading' });
  const [notes, setNotes] = useState<Readonly<Record<string, Note | undefined>>>({});
  const [dialog, setDialog] = useState<DialogState | null>(null);
  const [busy, setBusy] = useState(false);
  const alive = useRef(true);
  const acting = useRef(false);

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const load = useCallback(async () => {
    setState({ kind: 'loading' });
    try {
      const report = await actions.status(station);
      if (alive.current) setState({ kind: 'ready', report });
    } catch (e) {
      if (alive.current) {
        setState(
          e instanceof DeckError && e.status === 501 ? { kind: 'unsupported' } : { kind: 'error' },
        );
      }
    }
  }, [actions, station]);
  useEffect(() => {
    setNotes({});
    setDialog(null);
    void load();
  }, [load]);

  // The poll: every 3 s while an update runs, until `running` is `null`.
  const running = state.kind === 'ready' && state.report.running !== null;
  useEffect(() => {
    if (!running) return undefined;
    let stop = false;
    void pollUntilIdle(actions, station, {
      sleep,
      stopped: () => stop || !alive.current,
      onReport: (report) => setState({ kind: 'ready', report }),
    });
    return () => {
      stop = true;
    };
  }, [running, actions, station]);

  const note = (provider: ProviderKind, value: Note | undefined) =>
    setNotes((cur) => ({ ...cur, [provider]: value }));

  const check = guarded(acting, async (p: ProviderUpdateState) => {
    setBusy(true);
    note(p.provider, undefined);
    const out = await runCheck(actions, station, p);
    if (!alive.current) return;
    setBusy(false);
    if (out.kind === 'failed') {
      note(p.provider, { text: out.text, tone: 'error' });
      return;
    }
    note(p.provider, { text: out.line, tone: 'info' });
    setState((cur) =>
      cur.kind === 'ready'
        ? { kind: 'ready', report: withState(cur.report, out.answer.state, cur.report.running) }
        : cur,
    );
  });

  const update = guarded(acting, async (p: ProviderUpdateState) => {
    note(p.provider, undefined);
    const elevation = await askUpdate(ask);
    if (!alive.current || elevation === null) return;
    setDialog({ state: p, elevation, busy: false, sessions: null, force: false });
  });

  const confirm = guarded(acting, async () => {
    if (dialog === null) return;
    const { state: p } = dialog;
    const force = dialog.sessions?.canForce === true && dialog.force;
    setDialog({ ...dialog, busy: true });
    const out = await runUpdate(actions, station, p.provider, dialog.elevation, force);
    if (!alive.current) return;
    if (out.kind === 'busy') {
      setDialog({ ...dialog, busy: false, sessions: out.view });
      return;
    }
    setDialog(null);
    if (out.kind === 'started') {
      setState((cur) =>
        cur.kind === 'ready'
          ? {
              kind: 'ready',
              report: withState(cur.report, out.answer.state, out.answer.attempt_id),
            }
          : cur,
      );
    } else {
      note(p.provider, { text: out.text, tone: 'error' });
    }
  });

  return (
    <CliUpdateView
      station={station}
      state={state}
      pins={pins}
      now={Date.now()}
      notes={notes}
      busy={busy}
      dialog={dialog}
      onCheck={(p) => void check(p)}
      onUpdate={(p) => void update(p)}
      onRetry={() => void load()}
      onForce={(force) => setDialog((cur) => (cur === null ? cur : { ...cur, force }))}
      onConfirm={() => void confirm()}
      onClose={() => setDialog(null)}
    />
  );
}
