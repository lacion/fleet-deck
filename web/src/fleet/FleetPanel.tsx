import { useEffect, useRef, useState } from 'react';
import { repoName, repoNames, stationName } from '../board/fleet';
import type { DeckSession, GrantView, StationView, TaskView } from '../gen';
import { CircleDot, Icon, Mail, Plus } from '../ui/icons';
import {
  type FleetActions,
  type FleetResult,
  NOT_LIVE,
  nextSends,
  type SendEntry,
  type SendIntent,
  type SendRecord,
  type SendTarget,
  sameIntent,
  taskStatus,
} from './fleet';
import { FleetAccess } from './FleetAccess';

/** Mail that waits for delivery to one session (the card extras). */
export type MailWaiting = { readonly key: string; readonly name: string; readonly count: number };

type Props = {
  readonly actions: FleetActions | undefined;
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  /**
   * `tab` (default): the body of the Mail & tasks tab. `section`: an always-visible section of
   * the wide ops column (DESIGN-SYSTEM §4.4): a heading with a count, the waiting mail and the
   * tasks as rows, and the form folded under "New mail or task"; the Fleet access section of the
   * grants follows it as a sibling section.
   */
  readonly layout?: 'tab' | 'section' | undefined;
  /** The waiting mail rows of the section layout. */
  readonly waiting?: readonly MailWaiting[] | undefined;
  /** Section layout: each new value opens the "New mail or task" fold and focuses its text. */
  readonly composeSeq?: number | undefined;
  /**
   * A new value loads the tasks and the grants again, at most once in `LIST_MS` (a trailing load
   * takes the last change). The section stays mounted while the home is open, so it follows the
   * fleet (`fleetVersion`, which changes with every event of a session) to show the new task and
   * grant states.
   */
  readonly version?: unknown;
};

/** Who gets the mail: one session, a station, a repository, or everyone. */
type Audience = 'one' | 'station' | 'repo' | 'all';
const AUDIENCES: readonly { readonly id: Audience; readonly label: string }[] = [
  { id: 'one', label: 'One session' },
  { id: 'station', label: 'Station' },
  { id: 'repo', label: 'Repo' },
  { id: 'all', label: 'Everyone' },
];

type ManyResult = {
  readonly ok: number;
  readonly total: number;
  readonly failures: readonly { readonly title: string; readonly text: string }[];
};

const keyOf = (s: DeckSession) => `${s.station_id}/${s.summary.session_id}`;
const nameOf = (s: DeckSession) => s.summary.callsign ?? s.summary.title ?? s.summary.session_id;

/**
 * Loads the tasks and the grants into `onTasks` and `onGrants`. The returned function drops the
 * answers that come after it (an unmount, or a newer load).
 */
export function loadLists(
  actions: FleetActions,
  onTasks: (tasks: readonly TaskView[]) => void,
  onGrants: (grants: readonly GrantView[]) => void,
): () => void {
  let current = true;
  void actions.tasks().then((t) => {
    if (current) onTasks(t);
  });
  void actions.grants().then((g) => {
    if (current) onGrants(g);
  });
  return () => {
    current = false;
  };
}

/** The shortest time between two loads of the lists for a `version` change. */
export const LIST_MS = 10_000;

/** The clock of a `throttle`; tests give a fake one. */
export type ThrottleClock = {
  readonly now: () => number;
  readonly later: (run: () => void, ms: number) => ReturnType<typeof setTimeout>;
  readonly cancel: (timer: ReturnType<typeof setTimeout>) => void;
};

const realClock: ThrottleClock = {
  now: () => Date.now(),
  later: (run, ms) => setTimeout(run, ms),
  cancel: (timer) => clearTimeout(timer),
};

/**
 * Runs `run` at most once in `ms`: a `poke` runs it at once when `ms` passed since the last run,
 * else it schedules one trailing run at the end of the wait (more pokes in the wait add nothing).
 * `stop` drops a scheduled run.
 */
export function throttle(
  run: () => void,
  ms: number,
  clock: ThrottleClock = realClock,
): { readonly poke: () => void; readonly stop: () => void } {
  let last = Number.NEGATIVE_INFINITY;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const fire = () => {
    timer = undefined;
    last = clock.now();
    run();
  };
  return {
    poke: () => {
      if (timer !== undefined) return;
      const wait = last + ms - clock.now();
      if (wait <= 0) fire();
      else timer = clock.later(fire, wait);
    },
    stop: () => {
      if (timer !== undefined) clock.cancel(timer);
      timer = undefined;
    },
  };
}

/** Mail and tasks for one session of the fleet (M3 6-2 / 6-4). */
export function FleetPanel({
  actions,
  sessions,
  stations,
  layout = 'tab',
  waiting = [],
  composeSeq,
  version,
}: Props) {
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
  const [composeOpen, setComposeOpen] = useState(false);
  const textRef = useRef<HTMLTextAreaElement>(null);
  // The value at mount is not a request: a request is a change after it.
  const composeSeen = useRef(composeSeq);
  const composeFocus = useRef(false);
  useEffect(() => {
    if (composeSeq === undefined || composeSeq === composeSeen.current) return;
    composeSeen.current = composeSeq;
    composeFocus.current = true;
    setComposeOpen(true);
  }, [composeSeq]);
  // Focus the text after the fold is open (a closed fold hides its form).
  useEffect(() => {
    if (!composeOpen || !composeFocus.current) return;
    composeFocus.current = false;
    textRef.current?.focus();
  }, [composeOpen, composeSeq]);
  // The lists load at mount (and for new actions), then again for a new `version`, throttled.
  const lists = useRef<{ readonly poke: () => void } | null>(null);
  const listsVersion = useRef(version);
  useEffect(() => {
    if (actions === undefined) return undefined;
    let drop = () => {};
    const gate = throttle(() => {
      drop();
      drop = loadLists(actions, setTasks, setGrants);
    }, LIST_MS);
    lists.current = gate;
    gate.poke();
    return () => {
      gate.stop();
      drop();
      lists.current = null;
    };
  }, [actions]);
  useEffect(() => {
    if (Object.is(version, listsVersion.current)) return;
    listsVersion.current = version;
    lists.current?.poke();
  }, [version]);
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
  // The outcome of a send. The section layout shows it outside the fold, so that it shows while
  // the form is closed.
  const resultNote =
    result === null ? null : (
      <p className="fd-note" role="status">
        {result.text}
      </p>
    );
  const form = (
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
        <textarea
          ref={textRef}
          value={text}
          onChange={(event) => setText(event.target.value)}
          rows={2}
        />
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
      {layout === 'tab' ? resultNote : null}
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
            Send again retries the {owed.entries.filter((e) => e.state === 'failed').length} failed
            of the first {owed.entries.length} recipients.
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
  );
  const taskList =
    tasks.length === 0 ? null : (
      <ul className="fd-fleet-list" aria-label="Tasks">
        {tasks.map((t) => (
          <li key={t.task_id}>
            <span className="fd-task-title">{t.title}</span>{' '}
            <span className={`fd-task-state fd-task-${t.state}`}>{taskStatus(t)}</span>
          </li>
        ))}
      </ul>
    );
  // The grants: rows that open the session's "Fleet access" dialog, and "Give fleet access…".
  const access = (
    <FleetAccess
      sessions={sessions}
      stations={stations}
      grants={grants}
      actions={actions}
      layout={layout}
      onChanged={() => {
        void actions.grants().then(setGrants);
      }}
    />
  );
  if (layout === 'section') {
    const rows = waiting.length + tasks.length;
    return (
      <>
        <section className="fd-fleet fd-ops-sec" data-layout="section" aria-label="Mail and tasks">
          <h2 className="fd-ops-h">
            <Icon of={Mail} size={14} /> Mail &amp; tasks
            {rows === 0 ? null : <span className="fd-count">{rows}</span>}
          </h2>
          {waiting.length === 0 ? null : (
            <ul className="fd-ops-rows" aria-label="Mail waiting">
              {waiting.map((w) => (
                <li key={w.key}>
                  <Icon of={Mail} size={14} />
                  <span className="fd-ops-row-text">
                    <span className="fd-ops-row-title">
                      {w.count === 1 ? '1 mail waiting' : `${w.count} mails waiting`}
                    </span>
                    <span className="fd-ops-row-sub">For delivery to {w.name}</span>
                  </span>
                </li>
              ))}
            </ul>
          )}
          {tasks.length === 0 ? null : (
            <ul className="fd-fleet-list fd-ops-rows" aria-label="Tasks">
              {tasks.map((t) => (
                <li key={t.task_id}>
                  <Icon of={CircleDot} size={14} />
                  <span className="fd-ops-row-text">
                    <span className="fd-task-title fd-ops-row-title">{t.title}</span>
                    <span className={`fd-task-state fd-task-${t.state} fd-ops-row-sub`}>
                      {taskStatus(t)}
                    </span>
                  </span>
                </li>
              ))}
            </ul>
          )}
          <details
            className="fd-fleet-compose"
            open={composeOpen}
            onToggle={(e) => setComposeOpen(e.currentTarget.open)}
          >
            {/* The look of "Give fleet access…" (a leading plus); the chevron trails and turns. */}
            <summary data-variant="toggle">
              <Icon of={Plus} size={14} /> New mail or task
            </summary>
            {form}
          </details>
          {resultNote}
        </section>
        {access}
      </>
    );
  }
  return (
    <section className="fd-fleet" aria-label="Mail and tasks">
      {form}
      {taskList}
      {access}
    </section>
  );
}
