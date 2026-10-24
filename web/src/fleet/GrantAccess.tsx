// MCP P4b-4 (design P4b §4.6): give fleet access to a running session and change a live grant.
import { useState } from 'react';
import { stationName } from '../board/fleet';
import type { DeckSession, GrantView, StationView } from '../gen';
import type { ElevationAsk } from '../settings/elevation';
import {
  answerCapProblem,
  type FleetControl,
  fleetElevation,
  fleetProblem,
  grantFields,
  NO_FLEET,
  spawnCapProblem,
} from '../spawn/fleetControl';
import { AccessFields, CODEX_NOTE } from './AccessForm';
import { type FleetActions, type FleetResult, type GrantBody, NOT_LIVE } from './fleet';
import { NOT_ALLOWED } from './GrantSpawn';

export { CODEX_NOTE };
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

/**
 * The coordinator request of a start without the desktop, screenshot, recording, input and click
 * fields. `targets` are the picked stations other than the session's own.
 */
export function liveGrantFields(f: FleetControl, targets: readonly string[] = []): GrantBody {
  const g = grantFields(f, targets);
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
export const titleOf = (s: DeckSession) =>
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
 * cancelled dialog sends nothing and answers `null`. `targets` are the picked stations other than
 * the session's own.
 */
export async function giveAccess(
  session: DeckSession,
  fleet: FleetControl,
  actions: Pick<FleetActions, 'give'>,
  ask: ElevationAsk,
  targets: readonly string[] = [],
): Promise<FleetResult | null> {
  const gate = await fleetElevation({ coordinator: true, importing: false, fleet, targets }, ask);
  if (!gate.ok) return null;
  return actions.give(
    session.station_id,
    session.summary.session_id,
    liveGrantFields(fleet, targets),
    gate.elevation,
  );
}

type GiveProps = {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly grants: readonly GrantView[];
  readonly actions: FleetActions;
  readonly ask: ElevationAsk;
  /** Called after a give that the deck took, with its outcome (the grants load again). */
  readonly onChanged: (result: FleetResult) => void;
  /** Closes the dialog that holds the form. */
  readonly onClose: () => void;
};

/**
 * The body of the "Give fleet access" dialog of the ops panel: the running session that gets the
 * access, then the fields of the session menu's "Fleet access" dialog (`AccessFields`), so that one
 * task has one form.
 */
export function GiveAccess({
  sessions,
  stations,
  grants,
  actions,
  ask,
  onChanged,
  onClose,
}: GiveProps) {
  const [pick, setPick] = useState('');
  const [fleet, setFleet] = useState<FleetControl>(START_LIVE);
  const [picked, setPicked] = useState<readonly string[]>([]);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<FleetResult | null>(null);
  const candidates = giveCandidates(sessions, stations, grants);
  const chosen = candidates.find((s) => keyOf(s) === pick) ?? candidates[0];
  const problem = fleetProblem(fleet);
  if (chosen === undefined) {
    return (
      <>
        <p className="fd-note">{NONE_NOTE}</p>
        <div className="fd-access-foot">
          <button type="button" className="fd-ghost" onClick={onClose}>
            Close
          </button>
        </div>
      </>
    );
  }
  const give = () => {
    if (problem !== null || busy) return;
    setBusy(true);
    setResult(null);
    const targets = picked.filter((id) => id !== chosen.station_id);
    void giveAccess(chosen, fleet, actions, ask, targets)
      .then((r) => {
        if (r === null) return;
        if (!r.ok) {
          setResult(r);
          return;
        }
        setFleet(START_LIVE);
        setPicked([]);
        setPick('');
        onChanged(r);
      })
      .finally(() => setBusy(false));
  };
  return (
    <form
      className="fd-fleet-form fd-access-form"
      aria-label="Give fleet access"
      onSubmit={(event) => {
        event.preventDefault();
        give();
      }}
    >
      <section className="fd-access-sec">
        <div className="fd-access-field">
          <label>
            Session
            <select
              value={keyOf(chosen)}
              onChange={(event) => {
                setPick(event.target.value);
                setPicked([]);
              }}
            >
              {candidates.map((s) => (
                <option key={keyOf(s)} value={keyOf(s)}>
                  {`${titleOf(s)} on ${stationName(s.station_id, stations)}`}
                </option>
              ))}
            </select>
          </label>
        </div>
      </section>
      <AccessFields
        stations={stations}
        ownId={chosen.station_id}
        picked={picked}
        onPicked={setPicked}
        fleet={fleet}
        onFleet={setFleet}
        codex={chosen.summary.provider === 'codex'}
      />
      {result === null ? null : (
        <p className="fd-note fd-fleet-failed" role="alert">
          {result.text}
        </p>
      )}
      <div className="fd-access-foot">
        <button type="button" className="fd-ghost" disabled={busy} onClick={onClose}>
          Cancel
        </button>
        <button type="submit" className="fd-primary" disabled={busy || problem !== null}>
          Give access
        </button>
      </div>
    </form>
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
