// The fields of the fleet access forms: the dialog of the session menu and the Fleet page give form.
import { stationName } from '../board/fleet';
import type { StationView } from '../gen';
import { CLAUDE_CEILINGS, CODEX_CEILINGS } from '../spawn/FleetControlFields';
import { answerCapProblem, type FleetControl, spawnCapProblem } from '../spawn/fleetControl';
import { targetChoices } from '../spawn/TargetStations';

export const CODEX_NOTE = 'Codex sees the change when the session resumes.';
const ALWAYS_NOTE =
  'Always: read the fleet, send mail and tasks, and drive the sessions it starts.';
const LOCKED_NOTE = 'To change the stations, revoke the access and give it again.';
const CEILING_NOTE = 'Sessions that it starts, resumes or changes never get a higher mode.';

type PermissionKey = 'spawn' | 'scope' | 'inspect' | 'manage' | 'answer';

const PERMISSIONS: readonly {
  readonly key: PermissionKey;
  readonly title: string;
  readonly desc: string;
}[] = [
  { key: 'spawn', title: 'Start sessions', desc: 'Start new sessions on the picked stations.' },
  {
    key: 'scope',
    title: 'Whole fleet',
    desc: 'Reach every session on the picked stations, not only the sessions it starts.',
  },
  { key: 'inspect', title: 'Inspect', desc: 'Read diffs, files, artifacts and integrations.' },
  {
    key: 'manage',
    title: 'Manage',
    desc: 'Resume, rename, change the mode, and move sessions to a project.',
  },
  {
    key: 'answer',
    title: 'Answer approvals',
    desc: 'Allow once or deny approvals, and answer questions.',
  },
];

type StationsProps = {
  readonly stations: readonly StationView[];
  /** The station of the session: always in the grant, so it is not a choice. */
  readonly ownId: string;
  readonly picked: readonly string[];
  readonly onPicked: (ids: readonly string[]) => void;
  /** The stations cannot change: every box is disabled and a note says how to change them. */
  readonly locked?: boolean;
};

/** The "Stations" section: the own station (always on) and one box for each other station. */
export function AccessStations({
  stations,
  ownId,
  picked,
  onPicked,
  locked = false,
}: StationsProps) {
  return (
    <section className="fd-access-sec">
      <h4 className="fd-access-h">Stations</h4>
      <label className="fd-access-row">
        <input type="checkbox" checked disabled readOnly />
        <span className="fd-access-title">
          {stationName(ownId, stations)} <span className="fd-access-desc">this session</span>
        </span>
      </label>
      {targetChoices(stations, ownId).map((s) => (
        <label key={s.station_id} className="fd-access-row">
          <input
            type="checkbox"
            checked={picked.includes(s.station_id)}
            disabled={locked}
            onChange={(event) =>
              onPicked(
                event.target.checked
                  ? [...picked, s.station_id]
                  : picked.filter((id) => id !== s.station_id),
              )
            }
          />
          <span className="fd-access-title">{stationName(s.station_id, stations)}</span>
        </label>
      ))}
      {locked ? <p className="fd-note">{LOCKED_NOTE}</p> : null}
    </section>
  );
}

type FieldsProps = {
  readonly stations: readonly StationView[];
  readonly ownId: string;
  readonly picked: readonly string[];
  readonly onPicked: (ids: readonly string[]) => void;
  readonly fleet: FleetControl;
  readonly onFleet: (f: FleetControl) => void;
  /** True when the session is a Codex session. */
  readonly codex: boolean;
  readonly stationsLocked?: boolean;
  /** The classes that the grant already has (the edit mode): its limits show only for a newly added class. */
  readonly lockedClasses?: readonly string[];
};

type LimitProps = {
  readonly label: string;
  readonly value: string;
  readonly min: number;
  readonly max: number;
  readonly problem: string | null;
  readonly onChange: (value: string) => void;
};

function Limit({ label, value, min, max, problem, onChange }: LimitProps) {
  return (
    <div>
      <label>
        {label}
        <input
          type="number"
          inputMode="numeric"
          min={min}
          max={max}
          value={value}
          onChange={(event) => onChange(event.target.value)}
        />
      </label>
      {problem === null ? null : (
        <p className="fd-note" role="alert">
          {problem}
        </p>
      )}
    </div>
  );
}

type ModeProps = {
  readonly label: string;
  readonly value: string;
  readonly modes: readonly string[];
  readonly onChange: (value: string) => void;
};

function Mode({ label, value, modes, onChange }: ModeProps) {
  return (
    <div>
      <label>
        {label}
        <select value={value} onChange={(event) => onChange(event.target.value)}>
          {modes.map((mode) => (
            <option key={mode} value={mode}>
              {mode}
            </option>
          ))}
        </select>
      </label>
    </div>
  );
}

/** The stations, the permissions and the limits of a fleet access form. */
export function AccessFields({
  stations,
  ownId,
  picked,
  onPicked,
  fleet,
  onFleet,
  codex,
  stationsLocked = false,
  lockedClasses,
}: FieldsProps) {
  const editing = lockedClasses !== undefined;
  const newly = (key: PermissionKey): boolean =>
    fleet[key] && !(lockedClasses?.includes(key) ?? false);
  const showCap = newly('spawn');
  const showModes = newly('spawn') || newly('manage');
  const showAnswerCap = newly('answer');
  return (
    <>
      <AccessStations
        stations={stations}
        ownId={ownId}
        picked={picked}
        onPicked={onPicked}
        locked={stationsLocked}
      />
      <section className="fd-access-sec">
        <h4 className="fd-access-h">Permissions</h4>
        <p className="fd-note">{ALWAYS_NOTE}</p>
        {PERMISSIONS.filter((p) => !(editing && p.key === 'scope')).map((p) => (
          <label key={p.key} className="fd-access-row">
            <input
              type="checkbox"
              checked={fleet[p.key]}
              onChange={(event) => onFleet({ ...fleet, [p.key]: event.target.checked })}
            />
            <span className="fd-access-title">{p.title}</span>
            <span className="fd-access-desc">{p.desc}</span>
          </label>
        ))}
      </section>
      {showCap || showModes || showAnswerCap ? (
        <section className="fd-access-sec">
          <h4 className="fd-access-h">New sessions</h4>
          <div className="fd-access-grid">
            {showCap ? (
              <Limit
                label="Session limit"
                value={fleet.spawnCap}
                min={1}
                max={50}
                problem={spawnCapProblem(fleet)}
                onChange={(spawnCap) => onFleet({ ...fleet, spawnCap })}
              />
            ) : null}
            {showModes ? (
              <>
                <Mode
                  label="Highest Claude mode"
                  value={fleet.claudeCeiling}
                  modes={CLAUDE_CEILINGS}
                  onChange={(claudeCeiling) => onFleet({ ...fleet, claudeCeiling })}
                />
                <Mode
                  label="Highest Codex mode"
                  value={fleet.codexCeiling}
                  modes={CODEX_CEILINGS}
                  onChange={(codexCeiling) => onFleet({ ...fleet, codexCeiling })}
                />
              </>
            ) : null}
            {showAnswerCap ? (
              <Limit
                label="Answer limit"
                value={fleet.answerCap}
                min={0}
                max={5000}
                problem={answerCapProblem(fleet)}
                onChange={(answerCap) => onFleet({ ...fleet, answerCap })}
              />
            ) : null}
          </div>
          {showModes ? <p className="fd-note">{CEILING_NOTE}</p> : null}
        </section>
      ) : null}
      {codex ? <p className="fd-note">{CODEX_NOTE}</p> : null}
    </>
  );
}
