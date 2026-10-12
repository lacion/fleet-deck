import { useCallback, useEffect, useState } from 'react';
import type { DeckPlan, PlanStatus, SessionKey, StationView } from '../gen';
import { stationName } from './fleet';
import { type PlanActions, planTitle, STATUS_LABEL } from './plans';

type Props = {
  readonly stations: readonly StationView[];
  readonly actions: PlanActions | undefined;
  readonly live: boolean;
  /** Opens the session that proposed a plan. */
  readonly onOpen: (key: SessionKey) => void;
  /** Opens the spawn form with the plan as the first prompt. */
  readonly onExecute: (plan: DeckPlan) => void;
  /** The library at the first render (tests). */
  readonly initial?: readonly DeckPlan[] | null;
};

/** The plan library (v0 `PlanLibrary.tsx` parity, DL-301): newest first. */
export function PlanLibrary({ stations, actions, live, onOpen, onExecute, initial = null }: Props) {
  const [plans, setPlans] = useState<readonly DeckPlan[] | null>(initial);
  const [archived, setArchived] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const load = useCallback(() => {
    if (actions === undefined) return;
    setError(null);
    actions.list(archived).then(setPlans, (e: unknown) => {
      setError(e instanceof Error ? e.message : 'The deck did not answer.');
    });
  }, [actions, archived]);
  useEffect(load, [load]);
  const canDrive = live && actions?.canDrive === true;
  const mark = (plan: DeckPlan, status: PlanStatus) => {
    if (actions === undefined) return;
    setBusy(plan.id);
    setError(null);
    actions
      .mark(plan.id, status)
      .then(
        (row) => {
          setPlans((list) =>
            (list ?? [])
              .map((p) => (p.id === row.id ? row : p))
              .filter((p) => archived || p.status !== 'archived'),
          );
        },
        (e: unknown) => setError(e instanceof Error ? e.message : 'The deck did not answer.'),
      )
      .finally(() => setBusy(null));
  };
  return (
    <main className="fd-plans" aria-label="Plan library">
      <h2>Plans</h2>
      <div className="fd-plan-tools">
        <label>
          <input
            type="checkbox"
            checked={archived}
            onChange={(event) => setArchived(event.target.checked)}
          />{' '}
          Show archived
        </label>
        <button type="button" onClick={load} disabled={actions === undefined}>
          Refresh
        </button>
      </div>
      {error ? <p role="alert">{error}</p> : null}
      {plans === null ? (
        <p role="status">Loading plans…</p>
      ) : plans.length === 0 ? (
        <p className="fd-note">No plans yet. A session in plan mode adds its plan here.</p>
      ) : (
        <ol className="fd-plan-list">
          {plans.map((p) => {
            const title = planTitle(p.markdown);
            return (
              <li key={p.id} data-plan-status={p.status}>
                <article aria-label={`Plan ${title}`}>
                  <h3>{title}</h3>
                  <p className="fd-plan-meta">
                    <span className="fd-plan-status">{STATUS_LABEL[p.status]}</span> ·{' '}
                    {stationName(p.station_id, stations)} ·{' '}
                    <time dateTime={new Date(p.proposed_at).toISOString()}>
                      {new Date(p.proposed_at).toLocaleString()}
                    </time>{' '}
                    ·{' '}
                    <button
                      type="button"
                      className="fd-link"
                      onClick={() => onOpen({ station_id: p.station_id, session_id: p.session_id })}
                    >
                      {p.session_title ?? p.session_id}
                    </button>
                  </p>
                  <details>
                    <summary>Show plan</summary>
                    <pre className="fd-plan-text">{p.markdown}</pre>
                    {p.truncated ? (
                      <p className="fd-note">The deck keeps only the start of this long plan.</p>
                    ) : null}
                  </details>
                  <div className="fd-plan-actions">
                    <button type="button" disabled={!canDrive} onClick={() => onExecute(p)}>
                      Start a session with this plan
                    </button>
                    {p.status !== 'executed' ? (
                      <button
                        type="button"
                        disabled={!canDrive || busy === p.id}
                        onClick={() => mark(p, 'executed')}
                      >
                        Mark executed
                      </button>
                    ) : null}
                    {p.status === 'archived' ? (
                      <button
                        type="button"
                        disabled={!canDrive || busy === p.id}
                        onClick={() => mark(p, 'proposed')}
                      >
                        Restore
                      </button>
                    ) : (
                      <button
                        type="button"
                        disabled={!canDrive || busy === p.id}
                        onClick={() => mark(p, 'archived')}
                      >
                        Archive
                      </button>
                    )}
                  </div>
                </article>
              </li>
            );
          })}
        </ol>
      )}
    </main>
  );
}
