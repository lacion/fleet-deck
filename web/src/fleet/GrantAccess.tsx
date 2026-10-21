// MCP P4b-4 (design P4b §4.6): give fleet access to a running session and change a live grant.
import { useState } from 'react';
import { stationName } from '../board/fleet';
import type { DeckSession, GrantView, StationView } from '../gen';
import type { ElevationAsk } from '../settings/elevation';
import {
  CLAUDE_CEILINGS,
  CODEX_CEILINGS,
  FleetControlFields,
  LimitInput,
} from '../spawn/FleetControlFields';
import {
  answerCapProblem,
  type FleetControl,
  fleetElevation,
  fleetProblem,
  grantFields,
  NO_FLEET,
  spawnCapProblem,
} from '../spawn/fleetControl';
import { type FleetActions, type FleetResult, type GrantBody, NOT_LIVE } from './fleet';
import { NOT_ALLOWED } from './GrantSpawn';

export const CODEX_NOTE = 'Codex sees the change when the session resumes.';
const NONE_NOTE = 'No running session can receive fleet access.';

/** The classes that a live change can add or remove, in the order of the form. */
export const EDITABLE: readonly string[] = [
  'read',
  'mail',
  'drive',
  'spawn',
  'inspect',
  'manage',
  'answer',
];
/** The classes of a grant that only a start gives. */
export const FIXED: readonly string[] = ['desktop', 'capture', 'input'];

/** The fleet form of a live give: nothing on except the session limit, as the spawn form starts. */
export const START_LIVE: FleetControl = { ...NO_FLEET, spawn: true };

/** The coordinator request of a start without the desktop, screenshot, recording, input and click fields. */
export function liveGrantFields(f: FleetControl): GrantBody {
  const g = grantFields(f);
  return {
    classes: g.classes.filter((c) => !FIXED.includes(c)),
    ...(g.scope === undefined ? {} : { scope: g.scope }),
    ...(g.answer_cap === undefined ? {} : { answer_cap: g.answer_cap }),
    ...(g.stations === undefined ? {} : { stations: g.stations }),
    ...(g.spawn_cap === undefined ? {} : { spawn_cap: g.spawn_cap }),
    ...(g.max_modes === undefined ? {} : { max_modes: g.max_modes }),
  };
}

const keyOf = (s: DeckSession) => `${s.station_id}/${s.summary.session_id}`;
const titleOf = (s: DeckSession) =>
  s.summary.title === null || s.summary.title === '' ? s.summary.session_id : s.summary.title;

/** The running sessions of stations that can change grants live and that have no live grant. */
export function giveCandidates(
  sessions: readonly DeckSession[],
  stations: readonly StationView[],
  grants: readonly GrantView[],
): readonly DeckSession[] {
  const liveStations = new Set(
    stations.filter((s) => s.coordinator_live === true).map((s) => s.station_id),
  );
  return sessions.filter(
    (s) =>
      liveStations.has(s.station_id) &&
      !NOT_LIVE.includes(s.status) &&
      !grants.some(
        (g) =>
          g.live === true &&
          (g.state === 'active' || g.state === 'expired') &&
          g.station_id === s.station_id &&
          g.session_id === s.summary.session_id,
      ),
  );
}

/**
 * Gives the access of the form to a session. The elevation is asked as the spawn form asks it; a
 * cancelled dialog sends nothing and answers `null`.
 */
export async function giveAccess(
  session: DeckSession,
  fleet: FleetControl,
  actions: Pick<FleetActions, 'give'>,
  ask: ElevationAsk,
): Promise<FleetResult | null> {
  const gate = await fleetElevation({ coordinator: true, importing: false, fleet }, ask);
  if (!gate.ok) return null;
  return actions.give(
    session.station_id,
    session.summary.session_id,
    liveGrantFields(fleet),
    gate.elevation,
  );
}

type GiveProps = {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly grants: readonly GrantView[];
  readonly actions: FleetActions;
  readonly ask: ElevationAsk;
  /** Called after a give, to load the grants again. */
  readonly onChanged: () => void;
};

/** Gives fleet access to a running session that has no live grant. */
export function GiveAccess({ sessions, stations, grants, actions, ask, onChanged }: GiveProps) {
  const [pick, setPick] = useState('');
  const [fleet, setFleet] = useState<FleetControl>(START_LIVE);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<FleetResult | null>(null);
  const candidates = giveCandidates(sessions, stations, grants);
  const chosen = candidates.find((s) => keyOf(s) === pick) ?? candidates[0];
  const problem = fleetProblem(fleet);
  const give = () => {
    if (chosen === undefined || problem !== null || busy) return;
    setBusy(true);
    void giveAccess(chosen, fleet, actions, ask)
      .then((r) => {
        if (r === null) return;
        setResult(r);
        if (r.ok) {
          setFleet(START_LIVE);
          setPick('');
          onChanged();
        }
      })
      .finally(() => setBusy(false));
  };
  return (
    <section className="fd-fleet-form" aria-label="Give fleet access">
      <h3 className="fd-label">Give fleet access</h3>
      {chosen === undefined ? (
        <p className="fd-note">{NONE_NOTE}</p>
      ) : (
        <form
          className="fd-fleet-form"
          onSubmit={(event) => {
            event.preventDefault();
            give();
          }}
        >
          <label>
            Session
            <select value={keyOf(chosen)} onChange={(event) => setPick(event.target.value)}>
              {candidates.map((s) => (
                <option key={keyOf(s)} value={keyOf(s)}>
                  {`${titleOf(s)} on ${stationName(s.station_id, stations)}`}
                </option>
              ))}
            </select>
          </label>
          <FleetControlFields fleet={fleet} onChange={setFleet} live />
          {chosen.summary.provider === 'codex' ? <p className="fd-note">{CODEX_NOTE}</p> : null}
          <div className="fd-fleet-actions">
            <button type="submit" disabled={busy || problem !== null}>
              Give access
            </button>
          </div>
        </form>
      )}
      {result === null ? null : (
        <p className="fd-note" role="status">
          {result.text}
        </p>
      )}
    </section>
  );
}

/** The inputs of the change form. */
export type AccessValues = {
  /** The checked classes among `EDITABLE`. */
  readonly classes: readonly string[];
  readonly spawnCap: string;
  readonly claude: string;
  readonly codex: string;
  readonly answerCap: string;
};

/** The `changeAccess` body. */
export type AccessBody = Parameters<FleetActions['changeAccess']>[1];

/** The values of a grant, as the change form shows them. */
export function accessValues(g: GrantView): AccessValues {
  return {
    classes: g.classes.filter((c) => EDITABLE.includes(c)),
    spawnCap: NO_FLEET.spawnCap,
    claude: g.max_modes?.['claude'] ?? NO_FLEET.claudeCeiling,
    codex: g.max_modes?.['codex'] ?? NO_FLEET.codexCeiling,
    answerCap: NO_FLEET.answerCap,
  };
}

const added = (g: GrantView, v: AccessValues, c: string): boolean =>
  v.classes.includes(c) && !g.classes.includes(c);

/** True when any class is checked that the grant does not have. */
export const widens = (g: GrantView, v: AccessValues): boolean =>
  EDITABLE.some((c) => added(g, v, c));

/** True when the checked classes are the editable classes that the grant has. */
const unchanged = (g: GrantView, v: AccessValues): boolean =>
  EDITABLE.every((c) => v.classes.includes(c) === g.classes.includes(c));

/** The first problem of the limit of a newly checked class, or null. */
export function accessProblem(g: GrantView, v: AccessValues): string | null {
  const f: FleetControl = {
    ...NO_FLEET,
    spawn: added(g, v, 'spawn'),
    spawnCap: v.spawnCap,
    answer: added(g, v, 'answer'),
    answerCap: v.answerCap,
  };
  return spawnCapProblem(f) ?? answerCapProblem(f);
}

/** The body of the form values: all checked classes and the fixed ones, and the fields of newly added classes. */
export function accessBody(g: GrantView, v: AccessValues): AccessBody {
  const modes = added(g, v, 'spawn') || added(g, v, 'manage');
  return {
    classes: [
      ...EDITABLE.filter((c) => v.classes.includes(c)),
      ...g.classes.filter((c) => FIXED.includes(c)),
    ],
    ...(added(g, v, 'spawn') ? { spawn_cap: Number(v.spawnCap.trim()) } : {}),
    ...(modes
      ? {
          max_modes: {
            ...(v.claude === NOT_ALLOWED ? {} : { claude: v.claude }),
            ...(v.codex === NOT_ALLOWED ? {} : { codex: v.codex }),
          },
        }
      : {}),
    ...(added(g, v, 'answer') ? { answer_cap: Number(v.answerCap.trim()) } : {}),
  };
}

/**
 * Saves the access. A widening asks the elevation first (label `Fleet access`); a cancelled dialog
 * sends nothing and answers `null`. No change sends nothing.
 */
export async function saveAccess(
  g: GrantView,
  v: AccessValues,
  actions: Pick<FleetActions, 'changeAccess'>,
  ask: ElevationAsk,
): Promise<FleetResult | null> {
  if (unchanged(g, v)) return { ok: true, text: 'Nothing changed.' };
  const body = accessBody(g, v);
  if (!widens(g, v)) return actions.changeAccess(g.grant_id, body);
  const code = await ask(['Fleet access']);
  return code === null ? null : actions.changeAccess(g.grant_id, body, code);
}

type FormProps = {
  readonly grant: GrantView;
  readonly values: AccessValues;
  readonly busy: boolean;
  readonly codex: boolean;
  readonly onChange: (next: AccessValues) => void;
  readonly onSave: () => void;
  readonly onCancel: () => void;
};

/** The inline form of the change, prefilled from the grant. */
export function AccessForm({ grant, values, busy, codex, onChange, onSave, onCancel }: FormProps) {
  const problem = accessProblem(grant, values);
  const toggle = (c: string, on: boolean) =>
    onChange({
      ...values,
      classes: on ? [...values.classes, c] : values.classes.filter((x) => x !== c),
    });
  return (
    <form
      className="fd-fleet-form"
      aria-label="Change access"
      onSubmit={(event) => {
        event.preventDefault();
        if (problem === null && !busy) onSave();
      }}
    >
      {EDITABLE.map((c) => (
        <label key={c} className="fd-check">
          <input
            type="checkbox"
            checked={values.classes.includes(c)}
            onChange={(event) => toggle(c, event.target.checked)}
          />
          {c}
        </label>
      ))}
      {grant.classes
        .filter((c) => FIXED.includes(c))
        .map((c) => (
          <label key={c} className="fd-check">
            <input type="checkbox" checked disabled readOnly />
            {`${c} (given only at start)`}
          </label>
        ))}
      {added(grant, values, 'spawn') ? (
        <LimitInput
          label="Session limit"
          value={values.spawnCap}
          max={50}
          problem={spawnCapProblem({ ...NO_FLEET, spawn: true, spawnCap: values.spawnCap })}
          onChange={(spawnCap) => onChange({ ...values, spawnCap })}
        />
      ) : null}
      {added(grant, values, 'spawn') || added(grant, values, 'manage') ? (
        <>
          <label>
            Highest Claude mode
            <select
              value={values.claude}
              onChange={(event) => onChange({ ...values, claude: event.target.value })}
            >
              <option value={NOT_ALLOWED}>not allowed</option>
              {CLAUDE_CEILINGS.map((mode) => (
                <option key={mode} value={mode}>
                  {mode}
                </option>
              ))}
            </select>
          </label>
          <label>
            Highest Codex mode
            <select
              value={values.codex}
              onChange={(event) => onChange({ ...values, codex: event.target.value })}
            >
              <option value={NOT_ALLOWED}>not allowed</option>
              {CODEX_CEILINGS.map((mode) => (
                <option key={mode} value={mode}>
                  {mode}
                </option>
              ))}
            </select>
          </label>
          <p className="fd-note">
            Sessions that it starts, resumes or changes never get a higher mode. A higher value
            needs your elevation.
          </p>
        </>
      ) : null}
      {added(grant, values, 'answer') ? (
        <LimitInput
          label="Answer limit"
          value={values.answerCap}
          max={5000}
          problem={answerCapProblem({ ...NO_FLEET, answer: true, answerCap: values.answerCap })}
          onChange={(answerCap) => onChange({ ...values, answerCap })}
        />
      ) : null}
      {codex ? <p className="fd-note">{CODEX_NOTE}</p> : null}
      <div className="fd-fleet-actions">
        <button type="submit" disabled={busy || problem !== null}>
          Save
        </button>
        <button type="button" disabled={busy} onClick={onCancel}>
          Cancel
        </button>
      </div>
    </form>
  );
}

type ChangeProps = {
  readonly grant: GrantView;
  readonly actions: FleetActions;
  /** The elevation of the owner; `undefined` for a login that cannot change access. */
  readonly ask: ElevationAsk | undefined;
  /** True when the grant's session is a Codex session. */
  readonly codex: boolean;
  /** Called after a change, to load the grants again. */
  readonly onChanged: () => void;
};

/** The classes of one live grant. */
export function ChangeAccess({ grant, actions, ask, codex, onChanged }: ChangeProps) {
  const [editing, setEditing] = useState(false);
  const [values, setValues] = useState<AccessValues>(() => accessValues(grant));
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<FleetResult | null>(null);
  const save = () => {
    if (ask === undefined) return;
    setBusy(true);
    void saveAccess(grant, values, actions, ask)
      .then((r) => {
        if (r === null) return;
        setResult(r);
        if (r.ok) setEditing(false);
        onChanged();
      })
      .finally(() => setBusy(false));
  };
  return (
    <div className="fd-grant-desktop">
      {grant.live !== true || grant.state === 'revoked' || ask === undefined || editing ? null : (
        <button
          type="button"
          onClick={() => {
            setValues(accessValues(grant));
            setResult(null);
            setEditing(true);
          }}
        >
          Change access
        </button>
      )}
      {editing ? (
        <AccessForm
          grant={grant}
          values={values}
          busy={busy}
          codex={codex}
          onChange={setValues}
          onSave={save}
          onCancel={() => setEditing(false)}
        />
      ) : null}
      {result === null ? null : (
        <p className="fd-note" role="status">
          {result.text}
        </p>
      )}
    </div>
  );
}
