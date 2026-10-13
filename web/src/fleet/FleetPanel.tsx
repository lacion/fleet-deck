import { useEffect, useRef, useState } from 'react';
import { repoName, repoNames, stationName } from '../board/fleet';
import type { DeckSession, GrantView, StationView, TaskView } from '../gen';
import {
  type FleetActions,
  type FleetResult,
  grantStatus,
  nextSends,
  type SendEntry,
  type SendIntent,
  type SendRecord,
  type SendTarget,
  sameIntent,
  taskStatus,
} from './fleet';

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
  const sends = useRef<SendRecord | null>(null);
  const [, redraw] = useState(0);
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
  const toSend = (s: DeckSession): SendTarget => ({
    ref: keyOf(s),
    target: { stationId: s.station_id, sessionId: s.summary.session_id },
  });
  const selection =
    audience === 'station'
      ? stationId
      : audience === 'repo'
        ? repo
        : audience === 'one'
          ? chosen === undefined
            ? ''
            : keyOf(chosen)
          : '';
  const intent: SendIntent = { text: text.trim(), steer, audience, selection };
  const owed =
    audience !== 'one' &&
    sends.current !== null &&
    sameIntent(sends.current.intent, intent) &&
    sends.current.entries.some((e) => e.state === 'failed')
      ? sends.current
      : null;
  /** Sends what the record still owes, each mail with its own id, and keeps the record. */
  const deliver = async (list: readonly SendTarget[]) => {
    const { record, todo } = nextSends(sends.current, intent, list);
    sends.current = record;
    const results = new Map<string, FleetResult>();
    const done = new Map<string, SendEntry>();
    for (const e of todo) {
      let r: FleetResult;
      try {
        r = await actions.mail(e.target, record.intent.text, record.intent.steer, e.mailId);
      } catch (error) {
        r = { ok: false, text: error instanceof Error ? error.message : String(error) };
      }
      results.set(e.ref, r);
      done.set(e.ref, r.ok ? { ...e, state: 'sent' } : { ...e, state: 'failed', error: r.text });
    }
    const entries = record.entries.map((e) => done.get(e.ref) ?? e);
    sends.current = entries.every((e) => e.state === 'sent') ? null : { ...record, entries };
    return { entries, results };
  };
  const sendMany = async () => {
    if ((targets.length === 0 && owed === null) || text.trim() === '' || busy) return;
    setBusy(true);
    setMany(null);
    const names = new Map(sessions.map((s) => [keyOf(s), nameOf(s)]));
    const { entries } = await deliver(targets.map(toSend));
    const failed = entries.filter((e) => e.state === 'failed');
    setMany({
      ok: entries.length - failed.length,
      total: entries.length,
      failures: failed.map((e) => ({
        title: names.get(e.ref) ?? e.ref,
        text: e.error ?? '',
      })),
    });
    if (failed.length === 0) setText('');
    setBusy(false);
  };
  const sendOne = () => {
    if (chosen === undefined || text.trim() === '' || busy) return;
    setBusy(true);
    const ref = keyOf(chosen);
    void deliver([toSend(chosen)])
      .then(async ({ entries, results }) => {
        const r = results.get(ref);
        setResult(r ?? { ok: false, text: 'Mail not sent' });
        if (entries.every((e) => e.state === 'sent')) setText('');
        setTasks(await actions.tasks());
      })
      .finally(() => setBusy(false));
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
          if (audience === 'one') sendOne();
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
              (audience === 'one' ? chosen === undefined : targets.length === 0 && owed === null)
            }
          >
            {audience === 'one'
              ? 'Send mail'
              : targets.length === 0 && owed !== null
                ? 'Send again'
                : `Send to ${targets.length}`}
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
        {audience !== 'one' && targets.length === 0 && owed === null ? (
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
        {owed === null ? null : (
          <>
            <p className="fd-note">
              Send again retries the {owed.entries.filter((e) => e.state === 'failed').length}{' '}
              failed of the first {owed.entries.length} recipients.
            </p>
            <button
              type="button"
              className="fd-ghost fd-sm"
              onClick={() => {
                sends.current = null;
                setMany(null);
                setResult(null);
                redraw((n) => n + 1);
              }}
            >
              Start over
            </button>
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
