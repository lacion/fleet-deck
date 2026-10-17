// R3.4 and R3.7 (DL-164): "Start sessions" from issues, and "Review in a session" for a pull
// request. The station runs these starts in a restricted mode (the ticket text is untrusted).
import { useEffect, useRef, useState } from 'react';
import { formatHash } from '../app/routes';
import { newBatchId } from '../compare/compare';
import type { Project, StartedSession, StartedSessions, StationView } from '../gen';
import { Icon, X } from '../ui/icons';
import type { IntegrationActions } from './actions';
import { Dialog } from './Dialog';
import { ticketModes } from './model';
import { message } from './useLoad';

export const RESTRICTED_NOTE =
  'Sessions from tickets run in a restricted mode, because the ticket text is untrusted.';

/** The session that a receipt names, when the station already accepted the start. */
export const startedSession = (s: StartedSession): string | null =>
  s.command.receipt?.result?.session_id ?? null;

/** The answer of a start: one line for each started session and each failure. */
export function StartResult({
  station,
  result,
}: {
  readonly station: string;
  readonly result: StartedSessions;
}) {
  return (
    <div className="fd-int-result" role="status">
      <ul>
        {result.started.map((s, i) => {
          const session = startedSession(s);
          return (
            <li key={`${s.key}/${i}`} data-started={s.key}>
              {s.key} → started
              {session === null ? null : (
                <>
                  {' '}
                  <a href={formatHash({ name: 'session', station, session, tab: 'chat' })}>
                    Open the session
                  </a>
                </>
              )}
            </li>
          );
        })}
        {result.failed.map((f, i) => (
          <li key={`${f.key}/${i}`} data-failed={f.key} className="fd-int-fail">
            {f.key}: {f.message}
          </li>
        ))}
      </ul>
    </div>
  );
}

type Props = {
  readonly project: Project;
  readonly actions: IntegrationActions;
  readonly stations: readonly StationView[];
  /** The issue keys to start sessions for; empty for a review. */
  readonly issues: readonly string[];
  /** A pull request number to review in one session. */
  readonly review?: number | undefined;
  readonly onClose: () => void;
  /** The answer at the first render (tests). */
  readonly initialResult?: StartedSessions | undefined;
};

/** The start dialog: provider instance, mode, model and sessions per issue. */
export function StartDialog({
  project,
  actions,
  stations,
  issues,
  review,
  onClose,
  initialResult,
}: Props) {
  const providers =
    stations.find((s) => s.station_id === project.station_id)?.info?.providers ?? [];
  const [instance, setInstance] = useState(providers[0]?.instance ?? '');
  const [mode, setMode] = useState('');
  const [model, setModel] = useState('');
  const [count, setCount] = useState(1);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<StartedSessions | null>(initialResult ?? null);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const provider = providers.find((p) => p.instance === instance);
  const modes = ticketModes(provider?.provider);
  const reviewing = review !== undefined;
  const start = async () => {
    setBusy(true);
    setError(null);
    try {
      const done = await actions.startSessions(project.project_id, {
        batch_id: newBatchId(),
        issues: reviewing ? [] : [...issues],
        provider_instance: instance,
        mode: mode === '' ? null : mode,
        model: model.trim() === '' ? null : model.trim(),
        count_per_issue: reviewing ? 1 : count,
        review_of_pr: review ?? null,
      });
      if (alive.current) setResult(done);
    } catch (e) {
      if (alive.current) setError(message(e));
    } finally {
      if (alive.current) setBusy(false);
    }
  };
  const title = reviewing ? `Review #${review} in a session` : 'Start sessions';
  return (
    <Dialog title={title} onClose={onClose}>
      {result === null ? (
        <form
          className="fd-int-form"
          onSubmit={(e) => {
            e.preventDefault();
            void start();
          }}
        >
          <p className="fd-note">
            {reviewing ? `Pull request #${review}` : issues.join(', ')} · {project.name}
          </p>
          <label>
            Provider instance
            <select value={instance} onChange={(e) => setInstance(e.target.value)}>
              {providers.map((p) => (
                <option key={p.instance} value={p.instance}>
                  {p.instance}
                </option>
              ))}
            </select>
          </label>
          <label>
            Mode
            <select value={mode} onChange={(e) => setMode(e.target.value)}>
              <option value="">Most restrictive (default)</option>
              {modes.map((m) => (
                <option key={m} value={m}>
                  {m}
                </option>
              ))}
            </select>
          </label>
          <label>
            Model (optional)
            <input
              value={model}
              maxLength={200}
              autoComplete="off"
              onChange={(e) => setModel(e.target.value)}
            />
          </label>
          {reviewing ? null : (
            <label>
              Sessions per issue
              <input
                type="number"
                min={1}
                max={10}
                value={count}
                onChange={(e) => setCount(Math.min(10, Math.max(1, Number(e.target.value) || 1)))}
              />
            </label>
          )}
          <p className="fd-note">{RESTRICTED_NOTE}</p>
          {providers.length === 0 ? (
            <p className="fd-note">The station has not reported its providers yet.</p>
          ) : null}
          {error === null ? null : (
            <p className="fd-note fd-int-error" role="alert">
              {error}
            </p>
          )}
          <div className="fd-pane-row">
            <button type="submit" className="fd-primary" disabled={busy || instance === ''}>
              {busy ? 'Starting…' : 'Start'}
            </button>
            <button type="button" className="fd-ghost" onClick={onClose}>
              <Icon of={X} size={14} /> Cancel
            </button>
          </div>
        </form>
      ) : (
        <>
          <StartResult station={project.station_id} result={result} />
          <div className="fd-pane-row">
            <button type="button" className="fd-primary" onClick={onClose}>
              Close
            </button>
          </div>
        </>
      )}
    </Dialog>
  );
}
