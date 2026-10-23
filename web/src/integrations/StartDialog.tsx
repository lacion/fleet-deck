// R3.4 and R3.7 (DL-164): "Start sessions" from issues, and "Review in a session" for a pull
// request. The station runs these starts in a restricted mode (the ticket text is untrusted).
import { useEffect, useRef, useState } from 'react';
import { formatHash } from '../app/routes';
import { newBatchId } from '../compare/compare';
import type {
  Prefer,
  Project,
  ProjectHome,
  StartedSession,
  StartedSessions,
  StartSessions,
  StationView,
} from '../gen';
import { PlacementFields, StationChoice } from '../spawn/PlacementFields';
import { AUTO, autoOffered, parseNeeds } from '../spawn/spawn';
import type { IntegrationActions } from './actions';
import { Dialog } from './Dialog';
import { ticketModes } from './model';
import { message } from './useLoad';

export const RESTRICTED_NOTE =
  'Sessions from tickets run in a restricted mode, because the ticket text is untrusted.';

/** The session that a receipt names, when the station already accepted the start. */
export const startedSession = (s: StartedSession): string | null =>
  s.command.receipt?.result?.session_id ?? null;

/** The answer of a start: one line for each started session and each failure. `station` is the fallback. */
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
                  <a
                    href={formatHash({
                      name: 'session',
                      station: s.command.station_id ?? station,
                      session,
                      tab: 'chat',
                    })}
                  >
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

type ReadyHome = ProjectHome & { readonly path: string };
type Provider = NonNullable<StationView['info']>['providers'][number];

/** The ready homes of a project, in position order (an older deck sends no `homes`). */
export function readyHomes(project: Project): ReadyHome[] {
  return [...(project.homes ?? [])]
    .sort((a, b) => a.position - b.position)
    .filter((h): h is ReadyHome => h.state === 'ready' && h.path !== null);
}

/** The Home that the select starts with: the position-0 home when ready, else the first ready one. */
export const initialHome = (ready: readonly ReadyHome[]): string =>
  (ready.find((h) => h.position === 0) ?? ready[0])?.station_id ?? '';

/** Under Auto: the stations that have a ready home of the project and report their traits. */
export function autoStations(
  ready: readonly ReadyHome[],
  stations: readonly StationView[],
): StationView[] {
  return stations.filter((s) => ready.some((h) => h.station_id === s.station_id));
}

/**
 * The provider instances of a start: the station of the chosen home; under Auto the instances of
 * the stations with a ready home (no duplicates, in the order of first appearance). Without a
 * ready home: the station of the project.
 */
export function startProviders(
  project: Project,
  stations: readonly StationView[],
  ready: readonly ReadyHome[],
  home: string,
): Provider[] {
  const of = (id: string) => stations.find((s) => s.station_id === id)?.info?.providers ?? [];
  if (ready.length === 0) return [...of(project.station_id)];
  if (home !== AUTO) return [...of(home)];
  const seen = new Set<string>();
  const out: Provider[] = [];
  for (const h of ready) {
    for (const p of of(h.station_id)) {
      if (seen.has(p.instance)) continue;
      seen.add(p.instance);
      out.push(p);
    }
  }
  return out;
}

/** The body of a start. `home` is `null` when the dialog has no Home select. */
export function startBody(input: {
  readonly batchId: string;
  readonly issues: readonly string[];
  readonly review: number | undefined;
  readonly instance: string;
  readonly mode: string;
  readonly model: string;
  readonly count: number;
  readonly home: string | null;
  /** The station of the position-0 home: choosing it sends no `home`. */
  readonly primary: string | undefined;
  readonly needs: string;
  readonly prefer: readonly Prefer[];
  readonly spread: boolean;
}): StartSessions {
  const reviewing = input.review !== undefined;
  const batch = reviewing ? 1 : input.issues.length * input.count;
  const body: StartSessions = {
    batch_id: input.batchId,
    issues: reviewing ? [] : [...input.issues],
    provider_instance: input.instance,
    mode: input.mode === '' ? null : input.mode,
    model: input.model.trim() === '' ? null : input.model.trim(),
    count_per_issue: reviewing ? 1 : input.count,
    review_of_pr: input.review ?? null,
  };
  if (input.home === null || input.home === input.primary) return body;
  body.home = input.home;
  if (input.home === AUTO) {
    body.requires = parseNeeds(input.needs);
    body.prefer = [...input.prefer];
    if (batch >= 2) body.spread = input.spread;
  }
  return body;
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
  const ready = readyHomes(project);
  const byHome = ready.length > 0;
  const auto = autoOffered(autoStations(ready, stations));
  const [home, setHome] = useState(initialHome(ready));
  const [chosen, setInstance] = useState('');
  const [needs, setNeeds] = useState('');
  const [prefer, setPrefer] = useState<Prefer[]>([]);
  const [spread, setSpread] = useState(false);
  const providers = startProviders(project, stations, ready, home);
  // The chosen instance falls back to the first one when the list of the home does not have it.
  const instance = providers.some((p) => p.instance === chosen)
    ? chosen
    : (providers[0]?.instance ?? '');
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
  const placed = byHome && home === AUTO;
  const batch = reviewing ? 1 : issues.length * count;
  const start = async () => {
    setBusy(true);
    setError(null);
    try {
      const done = await actions.startSessions(
        project.project_id,
        startBody({
          batchId: newBatchId(),
          issues,
          review,
          instance,
          mode,
          model,
          count,
          home: byHome ? home : null,
          primary: ready.find((h) => h.position === 0)?.station_id,
          needs,
          prefer,
          spread,
        }),
      );
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
          {byHome ? (
            <>
              <StationChoice
                homes={ready}
                targets={stations}
                value={home}
                auto={auto}
                onChange={setHome}
              />
              {placed ? (
                <PlacementFields
                  needs={needs}
                  onNeeds={setNeeds}
                  prefer={prefer}
                  onPrefer={setPrefer}
                  batch={batch >= 2}
                  spread={spread}
                  onSpread={setSpread}
                />
              ) : null}
            </>
          ) : null}
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
              Cancel
            </button>
          </div>
        </form>
      ) : (
        <StartResult station={project.station_id} result={result} />
      )}
    </Dialog>
  );
}
