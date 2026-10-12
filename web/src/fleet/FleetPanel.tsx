import { useEffect, useState } from 'react';
import { repoName, repoNames, stationName } from '../board/fleet';
import type { DeckSession, GrantView, StationView, TaskView } from '../gen';
import { type FleetActions, type FleetResult, grantStatus, taskStatus } from './fleet';

type Props = {
  readonly actions: FleetActions | undefined;
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
};

/** Who gets the mail: one session, a station, a repository, or everyone. */
type Audience = 'one' | 'station' | 'repo' | 'all';
const AUDIENCES: readonly { readonly id: Audience; readonly label: string }[] = [
  { id: 'one', label: 'One session' },
  { id: 'station', label: 'Station' },
  { id: 'repo', label: 'Repo' },
  { id: 'all', label: 'Everyone' },
];
const NOT_LIVE: readonly DeckSession['status'][] = ['ended', 'lost', 'unreachable'];

type ManyResult = {
  readonly ok: number;
  readonly total: number;
  readonly failures: readonly { readonly title: string; readonly text: string }[];
};

const keyOf = (s: DeckSession) => `${s.station_id}/${s.summary.session_id}`;
const nameOf = (s: DeckSession) => s.summary.callsign ?? s.summary.title ?? s.summary.session_id;

/** Mail and tasks for one session of the fleet (M3 6-2 / 6-4). */
export function FleetPanel({ actions, sessions, stations }: Props) {
  const live = sessions.filter((s) => s.status !== 'ended');
  const [target, setTarget] = useState('');
  const [text, setText] = useState('');
  const [steer, setSteer] = useState(false);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<FleetResult | null>(null);
  const [audience, setAudience] = useState<Audience>('one');
  const [stationPick, setStationPick] = useState('');
  const [repoPick, setRepoPick] = useState('');
  const [many, setMany] = useState<ManyResult | null>(null);
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
  const reachable = sessions.filter((s) => !NOT_LIVE.includes(s.status));
  const stationIds = stations.map((s) => s.station_id);
  const stationId = stationIds.includes(stationPick) ? stationPick : (stationIds[0] ?? '');
  const repos = repoNames(reachable);
  const repo = repos.includes(repoPick) ? repoPick : (repos[0] ?? '');
  const targets =
    audience === 'station'
      ? reachable.filter((s) => s.station_id === stationId)
      : audience === 'repo'
        ? reachable.filter((s) => repoName(s) === repo)
        : audience === 'all'
          ? reachable
          : [];
  const pick = (next: Audience) => {
    setAudience(next);
    setMany(null);
    setResult(null);
  };
  const sendMany = async () => {
    if (targets.length === 0 || text.trim() === '' || busy) return;
    setBusy(true);
    setMany(null);
    let ok = 0;
    const failures: { title: string; text: string }[] = [];
    for (const t of targets) {
      const to = { stationId: t.station_id, sessionId: t.summary.session_id };
      try {
        const r = await actions.mail(to, text.trim(), steer);
        if (r.ok) ok += 1;
        else failures.push({ title: nameOf(t), text: r.text });
      } catch (e) {
        failures.push({ title: nameOf(t), text: e instanceof Error ? e.message : String(e) });
      }
    }
    setMany({ ok, total: targets.length, failures });
    if (failures.length === 0) setText('');
    setBusy(false);
  };
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
        className="fd-fleet-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (audience === 'one') run((to) => actions.mail(to, text.trim(), steer));
          else void sendMany();
        }}
      >
        <div className="fd-fleet-to" role="group" aria-label="To">
          <span className="fd-label">To</span>
          {AUDIENCES.map((a) => (
            <button
              key={a.id}
              type="button"
              className="fd-chip"
              aria-pressed={audience === a.id}
              onClick={() => pick(a.id)}
            >
              {a.label}
            </button>
          ))}
        </div>
        {audience === 'station' ? (
          <label>
            Station
            <select value={stationId} onChange={(e) => setStationPick(e.target.value)}>
              {stationIds.map((id) => (
                <option key={id} value={id}>
                  {stationName(id, stations)}
                </option>
              ))}
            </select>
          </label>
        ) : null}
        {audience === 'repo' ? (
          <label>
            Repo
            <select value={repo} onChange={(e) => setRepoPick(e.target.value)}>
              {repos.map((name) => (
                <option key={name} value={name}>
                  {name}
                </option>
              ))}
            </select>
          </label>
        ) : null}
        {audience !== 'one' ? null : (
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
        )}
        <label>
          Text
          <textarea value={text} onChange={(event) => setText(event.target.value)} rows={2} />
        </label>
        <label className="fd-check">
          <input type="checkbox" checked={steer} onChange={(e) => setSteer(e.target.checked)} />
          Steer now (mail only)
        </label>
        <div className="fd-fleet-actions">
          <button
            type="submit"
            disabled={
              busy ||
              text.trim() === '' ||
              (audience === 'one' ? chosen === undefined : targets.length === 0)
            }
          >
            {audience === 'one' ? 'Send mail' : `Send to ${targets.length}`}
          </button>
          <button
            type="button"
            disabled={busy || chosen === undefined || text.trim() === '' || audience !== 'one'}
            title={audience === 'one' ? undefined : 'Tasks go to one session'}
            onClick={() => run((to) => actions.task(to, text.trim()))}
          >
            Create task
          </button>
        </div>
        {audience !== 'one' && targets.length === 0 ? (
          <p className="fd-note">No live session matches.</p>
        ) : null}
        {result === null ? null : (
          <p className="fd-note" role="status">
            {result.text}
          </p>
        )}
        {many === null ? null : (
          <>
            <p className="fd-note" role="status">
              Sent to {many.ok} of {many.total}.
            </p>
            {many.failures.map((f, i) => (
              <p key={`${f.title}/${i}`} className="fd-note fd-fleet-failed" role="alert">
                {f.title}: {f.text}
              </p>
            ))}
          </>
        )}
      </form>
      {tasks.length === 0 ? null : (
        <ul className="fd-fleet-list" aria-label="Tasks">
          {tasks.map((t) => (
            <li key={t.task_id}>
              <span className="fd-task-title">{t.title}</span>{' '}
              <span className={`fd-task-state fd-task-${t.state}`}>{taskStatus(t)}</span>
            </li>
          ))}
        </ul>
      )}
      {grants.length === 0 ? null : (
        <ul className="fd-fleet-list" aria-label="Coordinator grants">
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
              {g.state === 'expired' && g.revoked_reason === 'idle' ? (
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => {
                    setBusy(true);
                    void actions
                      .renew(g.grant_id)
                      .then(async (r) => {
                        setResult(r);
                        setGrants(await actions.grants());
                      })
                      .finally(() => setBusy(false));
                  }}
                >
                  Renew
                </button>
              ) : null}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
