import { useCallback, useEffect, useRef, useState } from 'react';
import { relativeAge } from '../app/sessions';
import { stationName } from '../board/fleet';
import type { DeckSession, Schedule, ScheduleRun, ScheduleView, StationView } from '../gen';
import { spawnTargets } from '../spawn/spawn';
import type { ListWorkspaces } from '../ui/FolderPicker';
import { Icon, Plus, RefreshCw, Workflow } from '../ui/icons';
import { EmptyState, Page } from '../ui/page';
import { useNow } from '../ui/useNow';
import {
  type AutomationActions,
  type Draft,
  describeCronParts,
  draftOf,
  emptyDraft,
  inputOf,
  STATE_LABEL,
  STATE_TONE,
  untilText,
} from './automations';
import { ScheduleForm } from './ScheduleForm';

/** How often the page reads the list again (a run ends at the deck, not here). */
const REFRESH_MS = 20_000;
/** The reads after "Run now": the precheck and the start take a few seconds. */
const AFTER_RUN_MS = [3_000, 10_000];

type Props = {
  readonly stations: readonly StationView[];
  /** Absent without the owner login (no admin scope). */
  readonly actions: AutomationActions | undefined;
  readonly live: boolean;
  /** The list at the first render (tests). */
  readonly initial?: readonly ScheduleView[] | null;
  /** Unix ms of "now" (tests). */
  readonly now?: number;
  /** The form at the first render (tests): a draft for a new automation. */
  readonly initialForm?: Draft | null;
  /** Reads a folder of a station, for the folder picker of the form. */
  readonly list?: ListWorkspaces | undefined;
  readonly sessions?: readonly DeckSession[] | undefined;
};

const message = (e: unknown) => (e instanceof Error ? e.message : 'The deck did not answer.');

/** The full local date and time: the title of a relative time. */
const when = (ms: number) => new Date(ms).toLocaleString();

/** "3 h ago" (DESIGN-SYSTEM §5), "now" under a minute. */
const ago = (ms: number, now: number) => {
  const age = relativeAge(ms, now);
  return age === 'now' ? age : `${age} ago`;
};

function RunBadge({ run }: { readonly run: ScheduleRun }) {
  return (
    <span className="fd-badge fd-plan-status" data-tone={STATE_TONE[run.state]}>
      {STATE_LABEL[run.state]}
    </span>
  );
}

/** A past time as a relative age, with the full date in its title. */
function Ago({ at, now }: { readonly at: number; readonly now: number }) {
  return (
    <time dateTime={new Date(at).toISOString()} title={when(at)}>
      {ago(at, now)}
    </time>
  );
}

/** When an automation runs: a preset in words, a raw cron expression as code. */
function CronText({ cron }: { readonly cron: string }) {
  const parts = describeCronParts(cron);
  return 'label' in parts ? (
    parts.label
  ) : (
    <>
      cron <code>{parts.cron}</code>
    </>
  );
}

/** The newest runs of one automation. */
function RunHistory({
  id,
  actions,
  version,
  now,
}: {
  readonly id: string;
  readonly actions: AutomationActions;
  readonly version: number;
  readonly now: number;
}) {
  const [runs, setRuns] = useState<readonly ScheduleRun[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    // `version` is read so that a new value asks for a new read.
    if (version < 0) return undefined;
    let current = true;
    actions.runs(id).then(
      (list) => current && setRuns(list),
      (e: unknown) => current && setError(message(e)),
    );
    return () => {
      current = false;
    };
  }, [id, actions, version]);
  if (error !== null) return <p role="alert">{error}</p>;
  if (runs === null) return <p role="status">Loading runs…</p>;
  if (runs.length === 0) return <p className="fd-note">No runs yet.</p>;
  return (
    <ol className="fd-auto-runs">
      {runs.map((r) => (
        <li key={r.run_id} data-run-state={r.state}>
          <RunBadge run={r} /> <Ago at={r.due_at} now={now} />
          {r.manual ? ' · run now' : ''}
          {r.reason === '' ? '' : ` · ${r.reason}`}
          {r.session_id === null ? null : (
            <>
              {' · '}
              <a
                href={`#/s/${encodeURIComponent(r.station_id)}/${encodeURIComponent(r.session_id)}`}
              >
                Open the session
              </a>
            </>
          )}
        </li>
      ))}
    </ol>
  );
}

/** The automations page (R5, DL-168): schedules that the deck runs, with their history. */
export function AutomationsPage({
  stations,
  actions,
  live,
  initial = null,
  now: given,
  initialForm = null,
  list: listFolders,
  sessions,
}: Props) {
  const now = useNow(given, 30_000);
  const [list, setList] = useState<readonly ScheduleView[] | null>(initial);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [form, setForm] = useState<{ readonly id: string | null; readonly draft: Draft } | null>(
    initialForm === null ? null : { id: null, draft: initialForm },
  );
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [confirming, setConfirming] = useState<string | null>(null);
  const [open, setOpen] = useState<ReadonlySet<string>>(new Set());
  const [version, setVersion] = useState(0);
  const timers = useRef<ReturnType<typeof setTimeout>[]>([]);
  useEffect(() => () => timers.current.forEach(clearTimeout), []);
  const load = useCallback(() => {
    if (actions === undefined) return;
    actions.list().then(
      (rows) => {
        setList(rows);
        setError(null);
      },
      (e: unknown) => setError(message(e)),
    );
  }, [actions]);
  useEffect(load, [load]);
  useEffect(() => {
    if (actions === undefined) return undefined;
    const timer = setInterval(() => {
      load();
      setVersion((v) => v + 1);
    }, REFRESH_MS);
    return () => clearInterval(timer);
  }, [actions, load]);
  const can = live && actions !== undefined;
  const targets = spawnTargets(stations);

  const act = (id: string, what: () => Promise<unknown>, done?: string) => {
    setBusy(id);
    setError(null);
    what()
      .then(() => {
        if (done !== undefined) setNotice(done);
        load();
        setVersion((v) => v + 1);
      })
      .catch((e: unknown) => setError(message(e)))
      .finally(() => setBusy(null));
  };
  const runNow = (s: Schedule) => {
    if (actions === undefined) return;
    act(
      s.schedule_id,
      async () => {
        await actions.runNow(s.schedule_id);
        for (const ms of AFTER_RUN_MS) {
          timers.current.push(
            setTimeout(() => {
              load();
              setVersion((v) => v + 1);
            }, ms),
          );
        }
      },
      `A run of “${s.name}” started. Its result appears in the run history.`,
    );
  };
  const toggle = (s: Schedule) => {
    if (actions === undefined) return;
    const draft: Draft = { ...draftOf(s), enabled: !s.enabled };
    act(s.schedule_id, () => actions.update(s.schedule_id, inputOf(draft)));
  };
  const save = (draft: Draft) => {
    if (actions === undefined || form === null) return;
    setSaving(true);
    setSaveError(null);
    const body = inputOf(draft);
    const call = form.id === null ? actions.create(body) : actions.update(form.id, body);
    call
      .then(() => {
        setForm(null);
        load();
      })
      .catch((e: unknown) => setSaveError(message(e)))
      .finally(() => setSaving(false));
  };
  const first = targets[0];
  const newDraft = () =>
    emptyDraft(first?.station_id ?? '', first?.info?.providers[0]?.instance ?? '');
  // The one general primary of the screen (§2.2): in the header, or in the empty state while the
  // list is empty (one rule for every list page).
  const empty = list !== null && list.length === 0;
  const newAutomation = (
    <button
      type="button"
      className="fd-primary"
      disabled={!can || targets.length === 0}
      onClick={() => {
        setSaveError(null);
        setForm({ id: null, draft: newDraft() });
      }}
    >
      <Icon of={Plus} /> New automation
    </button>
  );
  return (
    <Page
      title="Automations"
      description="Sessions that start on a schedule."
      actions={
        <>
          {form === null && !empty ? newAutomation : null}
          {/* The board is live: Refresh is a quiet icon button after the primary. */}
          <button
            type="button"
            className="fd-iconbtn"
            aria-label="Refresh"
            title="Refresh"
            onClick={load}
            disabled={actions === undefined}
          >
            <Icon of={RefreshCw} />
          </button>
        </>
      }
      className="fd-auto"
      wide
    >
      {actions === undefined ? (
        <p className="fd-note">Automations need the owner’s login.</p>
      ) : targets.length === 0 && form === null ? (
        <p className="fd-note">No linked station has reported its providers yet.</p>
      ) : null}
      {error === null ? null : <p role="alert">{error}</p>}
      {notice === null ? null : <p role="status">{notice}</p>}
      {form === null ? null : (
        <ScheduleForm
          key={form.id ?? 'new'}
          stations={stations}
          initial={form.draft}
          error={saveError}
          saving={saving}
          editing={form.id !== null}
          onSave={save}
          onCancel={() => setForm(null)}
          list={listFolders}
          sessions={sessions}
        />
      )}
      {list === null ? (
        actions === undefined ? null : (
          <p role="status">Loading automations…</p>
        )
      ) : list.length === 0 ? (
        form === null ? (
          <EmptyState
            icon={Workflow}
            title="No automations yet"
            body="Run a triage, a nightly test fix or a review sweep on a schedule."
            action={newAutomation}
          />
        ) : null
      ) : (
        // The list sits on the canvas, the same as the Artifacts and Activity lists (no box).
        <ol className="fd-auto-list">
          {list.map(({ schedule: s, next_run_at: next, last_run: last }) => {
            const linked = stations.find((st) => st.station_id === s.station_id)?.reachable;
            return (
              <li key={s.schedule_id} data-enabled={s.enabled}>
                <article aria-label={`Automation ${s.name}`}>
                  <h3>
                    {s.name}{' '}
                    {s.enabled ? null : (
                      <span className="fd-badge" data-tone="muted">
                        Off
                      </span>
                    )}
                  </h3>
                  {/* The parts of the meta line, with the drawn dots of .fd-plan-meta between them. */}
                  <p className="fd-plan-meta">
                    <span>
                      <CronText cron={s.cron} />
                    </span>
                    <span>{s.timezone}</span>
                    <span>{stationName(s.station_id, stations)}</span>
                    {/* A span: the drawn dot before it stays in the sans of the line. A phone
                        puts it on its own line (views.css .fd-auto-path). */}
                    <span className="fd-auto-path">
                      <code>{s.path}</code>
                    </span>
                  </p>
                  <p className="fd-plan-meta">
                    <span className="fd-auto-when">
                      Next run:{' '}
                      {next === null ? (
                        'none (off)'
                      ) : (
                        <time dateTime={new Date(next).toISOString()} title={when(next)}>
                          {untilText(next, now)}
                        </time>
                      )}
                    </span>
                    <span className="fd-auto-when">
                      Last run:{' '}
                      {last === null ? (
                        'never'
                      ) : (
                        <>
                          <RunBadge run={last} />
                          {last.reason === '' ? null : <span>{last.reason}</span>}
                          <Ago at={last.due_at} now={now} />
                        </>
                      )}
                    </span>
                  </p>
                  <details
                    open={open.has(s.schedule_id)}
                    onToggle={(event) => {
                      const isOpen = event.currentTarget.open;
                      setOpen((cur) => {
                        const next = new Set(cur);
                        if (isOpen) next.add(s.schedule_id);
                        else next.delete(s.schedule_id);
                        return next;
                      });
                    }}
                  >
                    <summary>Run history</summary>
                    {open.has(s.schedule_id) && actions !== undefined ? (
                      <RunHistory
                        id={s.schedule_id}
                        actions={actions}
                        version={version}
                        now={now}
                      />
                    ) : null}
                  </details>
                  <div className="fd-plan-actions">
                    <button
                      type="button"
                      className="fd-sm"
                      disabled={!can || busy === s.schedule_id || linked === false}
                      title={linked === false ? 'The station is not linked.' : undefined}
                      onClick={() => runNow(s)}
                    >
                      Run now
                    </button>
                    <button
                      type="button"
                      className="fd-sm"
                      disabled={!can || form !== null}
                      onClick={() => {
                        setSaveError(null);
                        setForm({ id: s.schedule_id, draft: draftOf(s) });
                      }}
                    >
                      Edit
                    </button>
                    <button
                      type="button"
                      className="fd-ghost fd-sm"
                      disabled={!can || busy === s.schedule_id}
                      onClick={() => toggle(s)}
                    >
                      {s.enabled ? 'Turn off' : 'Turn on'}
                    </button>
                    {confirming === s.schedule_id ? (
                      <>
                        <span>Delete this automation and its history?</span>
                        <button
                          type="button"
                          className="fd-danger fd-sm"
                          disabled={!can || busy === s.schedule_id}
                          onClick={() => {
                            setConfirming(null);
                            const a = actions;
                            if (a !== undefined) act(s.schedule_id, () => a.remove(s.schedule_id));
                          }}
                        >
                          Delete
                        </button>
                        <button
                          type="button"
                          className="fd-ghost fd-sm"
                          onClick={() => setConfirming(null)}
                        >
                          Keep
                        </button>
                      </>
                    ) : (
                      <button
                        type="button"
                        className="fd-danger fd-sm"
                        disabled={!can}
                        onClick={() => setConfirming(s.schedule_id)}
                      >
                        Delete…
                      </button>
                    )}
                  </div>
                </article>
              </li>
            );
          })}
        </ol>
      )}
    </Page>
  );
}
