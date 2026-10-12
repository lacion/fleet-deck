import { useEffect, useState } from 'react';
import type { DeckSession, GrantView, TaskView } from '../gen';
import { type FleetActions, type FleetResult, grantStatus, taskStatus } from './fleet';

type Props = {
  readonly actions: FleetActions | undefined;
  readonly sessions: readonly DeckSession[];
};

const keyOf = (s: DeckSession) => `${s.station_id}/${s.summary.session_id}`;
const nameOf = (s: DeckSession) => s.summary.callsign ?? s.summary.title ?? s.summary.session_id;

/** Mail and tasks for one session of the fleet (M3 6-2 / 6-4). */
export function FleetPanel({ actions, sessions }: Props) {
  const live = sessions.filter((s) => s.status !== 'ended');
  const [target, setTarget] = useState('');
  const [text, setText] = useState('');
  const [steer, setSteer] = useState(false);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<FleetResult | null>(null);
  const [tasks, setTasks] = useState<readonly TaskView[]>([]);
  const [grants, setGrants] = useState<readonly GrantView[]>([]);
  useEffect(() => {
    if (actions === undefined) return undefined;
    let current = true;
    void actions.tasks().then((t) => {
      if (current) setTasks(t);
    });
    void actions.grants().then((g) => {
      if (current) setGrants(g);
    });
    return () => {
      current = false;
    };
  }, [actions]);
  if (actions === undefined) return null;
  const chosen = live.find((s) => keyOf(s) === target) ?? live[0];
  const run = (send: (to: { stationId: string; sessionId: string }) => Promise<FleetResult>) => {
    if (chosen === undefined || text.trim() === '' || busy) return;
    setBusy(true);
    void send({ stationId: chosen.station_id, sessionId: chosen.summary.session_id })
      .then(async (r) => {
        setResult(r);
        if (r.ok) setText('');
        setTasks(await actions.tasks());
      })
      .finally(() => setBusy(false));
  };
  return (
    <section className="fd-fleet" aria-label="Mail and tasks">
      <form
        onSubmit={(event) => {
          event.preventDefault();
          run((to) => actions.mail(to, text.trim(), steer));
        }}
      >
        <label>
          Session
          <select
            value={chosen === undefined ? '' : keyOf(chosen)}
            onChange={(e) => setTarget(e.target.value)}
          >
            {live.map((s) => (
              <option key={keyOf(s)} value={keyOf(s)}>
                {nameOf(s)}
              </option>
            ))}
          </select>
        </label>
        <label>
          Text
          <textarea value={text} onChange={(event) => setText(event.target.value)} rows={2} />
        </label>
        <label className="fd-check">
          <input type="checkbox" checked={steer} onChange={(e) => setSteer(e.target.checked)} />
          Steer now (mail only)
        </label>
        <div className="fd-fleet-actions">
          <button type="submit" disabled={busy || chosen === undefined || text.trim() === ''}>
            Send mail
          </button>
          <button
            type="button"
            disabled={busy || chosen === undefined || text.trim() === ''}
            onClick={() => run((to) => actions.task(to, text.trim()))}
          >
            Create task
          </button>
        </div>
        {result === null ? null : <p role="status">{result.text}</p>}
      </form>
      {tasks.length === 0 ? null : (
        <ul aria-label="Tasks">
          {tasks.map((t) => (
            <li key={t.task_id}>
              <span className="fd-task-title">{t.title}</span>{' '}
              <span className={`fd-task-state fd-task-${t.state}`}>{taskStatus(t)}</span>
            </li>
          ))}
        </ul>
      )}
      {grants.length === 0 ? null : (
        <ul aria-label="Coordinator grants">
          {grants.map((g) => (
            <li key={g.grant_id}>
              <span className="fd-task-title">Coordinator {g.session_id}</span>{' '}
              <span className={`fd-task-state fd-grant-${g.state}`}>{grantStatus(g)}</span>
              {g.state === 'revoked' ? null : (
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => {
                    setBusy(true);
                    void actions
                      .revoke(g.grant_id)
                      .then(async (r) => {
                        setResult(r);
                        setGrants(await actions.grants());
                      })
                      .finally(() => setBusy(false));
                  }}
                >
                  Revoke
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
