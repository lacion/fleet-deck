import { type FormEvent, useCallback, useEffect, useState } from 'react';
import { relativeAge, sessionTitle } from '../app/sessions';
import type { DeckPlan, DeckSession, PlanStatus, SessionKey, StationView } from '../gen';
import { FileText, Icon, RefreshCw } from '../ui/icons';
import { sessionRef } from '../ui/localSet';
import { EmptyState, Page } from '../ui/page';
import { useNow } from '../ui/useNow';
import { stationName } from './fleet';
import { liveProposer, type PlanActions, PROPOSER_NOTE, planTitle, STATUS_LABEL } from './plans';

/** The mail of the fleet that carries a plan to a live session. */
type Assign = (
  target: { stationId: string; sessionId: string },
  text: string,
) => Promise<{ ok: boolean; text: string }>;

/** "3 h ago" (DESIGN-SYSTEM §5), "now" under a minute. */
const ago = (ms: number, now: number) => {
  const age = relativeAge(ms, now);
  return age === 'now' ? age : `${age} ago`;
};

const isLiveSession = (card: DeckSession) =>
  card.status !== 'ended' && card.status !== 'lost' && card.status !== 'unreachable';

type Props = {
  readonly stations: readonly StationView[];
  /** The sessions that can receive a plan. */
  readonly sessions?: readonly DeckSession[];
  readonly actions: PlanActions | undefined;
  readonly live: boolean;
  /** Opens the session that proposed a plan. */
  readonly onOpen: (key: SessionKey) => void;
  /** Opens the spawn form with the plan as the first prompt. */
  readonly onExecute: (plan: DeckPlan) => void;
  /** Sends a plan to a live session as mail; absent when the deck has no fleet calls. */
  readonly onAssign?: Assign | undefined;
  /** The library at the first render (tests). */
  readonly initial?: readonly DeckPlan[] | null;
};

/** The badge tone of each status: a proposed plan waits (blue), an executed one is done (green). */
const STATUS_TONE: Readonly<Record<PlanStatus, string>> = {
  proposed: 'info',
  executed: 'ok',
  archived: 'muted',
};

const first = (cards: readonly DeckSession[]): string => {
  const card = cards[0];
  return card === undefined ? '' : sessionRef(card.station_id, card.summary.session_id);
};

/** The plan library (v0 `PlanLibrary.tsx` parity, DL-301): newest first. */
export function PlanLibrary({
  stations,
  sessions = [],
  actions,
  live,
  onOpen,
  onExecute,
  onAssign,
  initial = null,
}: Props) {
  const [plans, setPlans] = useState<readonly DeckPlan[] | null>(initial);
  const now = useNow(undefined, 30_000);
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
  const [assigning, setAssigning] = useState<{
    readonly plan: string;
    readonly target: string;
    readonly instructions: string;
    readonly error: string | null;
  } | null>(null);
  const [sent, setSent] = useState<{ readonly plan: string; readonly text: string } | null>(null);
  const targets = sessions.filter(isLiveSession);
  const assign = (plan: DeckPlan, event: FormEvent) => {
    event.preventDefault();
    if (actions === undefined || onAssign === undefined || assigning === null) return;
    const card = targets.find(
      (c) => sessionRef(c.station_id, c.summary.session_id) === assigning.target,
    );
    if (card === undefined) return;
    const instructions = assigning.instructions.trim();
    const text = `A plan from the library for you to execute.\n\n${plan.markdown}${
      instructions === '' ? '' : `\n\nInstructions: ${instructions}`
    }`;
    setBusy(plan.id);
    onAssign({ stationId: card.station_id, sessionId: card.summary.session_id }, text)
      .then(async (result) => {
        if (!result.ok) {
          setAssigning((a) => (a === null ? a : { ...a, error: result.text }));
          return;
        }
        setAssigning(null);
        setSent({ plan: plan.id, text: `Sent to ${sessionTitle(card)}.` });
        try {
          await actions.mark(plan.id, 'executed');
          load();
        } catch (e: unknown) {
          setError(e instanceof Error ? e.message : 'The deck did not answer.');
        }
      })
      .catch((e: unknown) =>
        setAssigning((a) =>
          a === null ? a : { ...a, error: e instanceof Error ? e.message : 'Mail not sent' },
        ),
      )
      .finally(() => setBusy(null));
  };
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
    <Page
      wide
      title="Plans"
      label="Plan library"
      description="Plans that sessions proposed. Start one as a new session."
      actions={
        <>
          <label className="fd-checkrow">
            <input
              type="checkbox"
              checked={archived}
              onChange={(event) => setArchived(event.target.checked)}
            />
            Show archived
          </label>
          <button
            type="button"
            className="fd-ghost"
            onClick={load}
            disabled={actions === undefined}
          >
            <Icon of={RefreshCw} /> Refresh
          </button>
        </>
      }
    >
      {error ? <p role="alert">{error}</p> : null}
      {plans === null ? (
        <p role="status">Loading plans…</p>
      ) : plans.length === 0 ? (
        <EmptyState
          icon={FileText}
          title="No plans yet"
          body="A session in plan mode adds its plan here."
        />
      ) : (
        <ol className="fd-panel fd-plan-list">
          {plans.map((p) => {
            const title = planTitle(p.markdown);
            const proposer = p.status === 'proposed' ? liveProposer(p, sessions) : undefined;
            return (
              <li key={p.id} data-plan-status={p.status}>
                <article aria-label={`Plan ${title}`}>
                  <h3>{title}</h3>
                  <p className="fd-plan-meta">
                    <span className="fd-badge fd-plan-status" data-tone={STATUS_TONE[p.status]}>
                      {STATUS_LABEL[p.status]}
                    </span>
                    <span>{stationName(p.station_id, stations)}</span>
                    <time
                      dateTime={new Date(p.proposed_at).toISOString()}
                      title={new Date(p.proposed_at).toLocaleString()}
                    >
                      {ago(p.proposed_at, now)}
                    </time>
                    <button
                      type="button"
                      className="fd-link"
                      onClick={() => onOpen({ station_id: p.station_id, session_id: p.session_id })}
                    >
                      {p.session_title ?? p.session_id}
                    </button>
                  </p>
                  <details className="fd-plan-fold">
                    <summary className="fd-tap">Show plan</summary>
                    <pre className="fd-plan-text">{p.markdown}</pre>
                    {p.truncated ? (
                      <p className="fd-note">The deck keeps only the start of this long plan.</p>
                    ) : null}
                  </details>
                  <div className="fd-plan-actions">
                    <button
                      type="button"
                      className="fd-sm"
                      disabled={!canDrive}
                      onClick={() => onExecute(p)}
                    >
                      Start a session with this plan
                    </button>
                    {p.status !== 'executed' ? (
                      <button
                        type="button"
                        className="fd-ghost fd-sm"
                        disabled={!canDrive || busy === p.id}
                        onClick={() => mark(p, 'executed')}
                      >
                        Mark executed
                      </button>
                    ) : null}
                    {p.status === 'archived' ? (
                      <button
                        type="button"
                        className="fd-ghost fd-sm"
                        disabled={!canDrive || busy === p.id}
                        onClick={() => mark(p, 'proposed')}
                      >
                        Restore
                      </button>
                    ) : (
                      <button
                        type="button"
                        className="fd-ghost fd-sm"
                        disabled={!canDrive || busy === p.id}
                        onClick={() => mark(p, 'archived')}
                      >
                        Archive
                      </button>
                    )}
                    {onAssign !== undefined && p.status === 'proposed' ? (
                      <button
                        type="button"
                        className="fd-ghost fd-sm"
                        disabled={!live}
                        onClick={() =>
                          setAssigning({
                            plan: p.id,
                            target: first(targets),
                            instructions: '',
                            error: null,
                          })
                        }
                      >
                        Assign to a session…
                      </button>
                    ) : null}
                  </div>
                  {proposer === undefined ? null : (
                    <p className="fd-note">
                      {PROPOSER_NOTE.before}
                      {proposer.summary.callsign == null ? (
                        sessionTitle(proposer)
                      ) : (
                        <span className="fd-callsign">{proposer.summary.callsign}</span>
                      )}
                      {PROPOSER_NOTE.after}
                    </p>
                  )}
                  {sent !== null && sent.plan === p.id ? <p role="status">{sent.text}</p> : null}
                  {assigning !== null && assigning.plan === p.id ? (
                    <form className="fd-plan-assign" onSubmit={(event) => assign(p, event)}>
                      <label>
                        Session
                        <select
                          value={assigning.target}
                          disabled={targets.length === 0}
                          onChange={(event) =>
                            setAssigning({ ...assigning, target: event.target.value })
                          }
                        >
                          {targets.map((c) => {
                            const ref = sessionRef(c.station_id, c.summary.session_id);
                            return (
                              <option key={ref} value={ref}>
                                {sessionTitle(c)} · {stationName(c.station_id, stations)}
                              </option>
                            );
                          })}
                        </select>
                      </label>
                      {targets.length === 0 ? (
                        <p className="fd-note">No live session to assign to.</p>
                      ) : null}
                      <label>
                        Instructions (optional)
                        <textarea
                          rows={3}
                          value={assigning.instructions}
                          onChange={(event) =>
                            setAssigning({ ...assigning, instructions: event.target.value })
                          }
                        />
                      </label>
                      {assigning.error === null ? null : <p role="alert">{assigning.error}</p>}
                      <div className="fd-plan-actions">
                        <button
                          type="submit"
                          className="fd-primary"
                          disabled={busy === p.id || assigning.target === ''}
                        >
                          Send as mail
                        </button>
                        <button
                          type="button"
                          className="fd-ghost"
                          onClick={() => setAssigning(null)}
                        >
                          Cancel
                        </button>
                      </div>
                    </form>
                  ) : null}
                </article>
              </li>
            );
          })}
        </ol>
      )}
    </Page>
  );
}
