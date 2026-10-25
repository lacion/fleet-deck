// The fields of the fleet access forms: the dialog of the session menu and the Fleet page give form.
import { stationName } from '../board/fleet';
import type { StationView } from '../gen';
import { CLAUDE_CEILINGS, CODEX_CEILINGS } from '../spawn/FleetControlFields';
import { answerCapProblem, type FleetControl, spawnCapProblem } from '../spawn/fleetControl';
import { targetChoices } from '../spawn/TargetStations';
import { ceilingLabel } from './modes';

export const CODEX_NOTE = 'Codex sees the change when the session resumes.';
const ALWAYS_NOTE =
  'Always: read the fleet, send mail and tasks, and drive the sessions it starts.';
const LOCKED_NOTE = 'To change the stations, revoke the access and give it again.';
const CEILING_NOTE = 'Sessions that it starts, resumes or changes never get a higher mode.';
const OWN_NOTE = 'The station of this session: always in the access.';

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

/** One row of the form: the title (and its description under it) at the left, the switch at the
 * right, on one line (DESIGN-SYSTEM §4.7: no box in a box, a rule between the rows). */
function SwitchRow({
  title,
  desc,
  checked,
  disabled = false,
  onChange,
}: {
  readonly title: string;
  readonly desc?: string | undefined;
  readonly checked: boolean;
  readonly disabled?: boolean;
  readonly onChange?: ((on: boolean) => void) | undefined;
}) {
  return (
    <label className="fd-access-row">
      <span className="fd-access-text">
        <span className="fd-access-title">{title}</span>
        {desc === undefined ? null : <span className="fd-access-desc">{desc}</span>}
      </span>
      <input
        type="checkbox"
        role="switch"
        className="fd-switch"
        aria-checked={checked}
        checked={checked}
        disabled={disabled}
        readOnly={onChange === undefined}
        onChange={onChange === undefined ? undefined : (event) => onChange(event.target.checked)}
      />
    </label>
  );
}

/** The "Stations" section: the own station (always on) and one switch for each other station. */
export function AccessStations({
  stations,
  ownId,
  picked,
  onPicked,
  locked = false,
}: StationsProps) {
  return (
    <section className="fd-access-sec">
      <h4 className="fd-label fd-access-h">Stations</h4>
      <div className="fd-access-rows">
        <SwitchRow title={stationName(ownId, stations)} desc={OWN_NOTE} checked disabled />
        {targetChoices(stations, ownId).map((s) => (
          <SwitchRow
            key={s.station_id}
            title={stationName(s.station_id, stations)}
            checked={picked.includes(s.station_id)}
            disabled={locked}
            onChange={(on) =>
              onPicked(on ? [...picked, s.station_id] : picked.filter((id) => id !== s.station_id))
            }
          />
        ))}
      </div>
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
  readonly problem: string | null;
  readonly onChange: (value: string) => void;
};

/** A limit input. No upper bound: the deck checks the configured limit (B27 §3.7). */
function Limit({ label, value, min, problem, onChange }: LimitProps) {
  return (
    <div className="fd-access-field">
      <label>
        {label}
        <input
          type="number"
          inputMode="numeric"
          min={min}
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

/** A mode ceiling: the options say the mode in words (`ceilingLabel`); the value stays the id. */
function Mode({ label, value, modes, onChange }: ModeProps) {
  return (
    <div className="fd-access-field">
      <label>
        {label}
        <select value={value} onChange={(event) => onChange(event.target.value)}>
          {modes.map((mode) => (
            <option key={mode} value={mode}>
              {ceilingLabel(mode)}
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
        <h4 className="fd-label fd-access-h">Permissions</h4>
        <p className="fd-note">{ALWAYS_NOTE}</p>
        <div className="fd-access-rows">
          {PERMISSIONS.filter((p) => !(editing && p.key === 'scope')).map((p) => (
            <SwitchRow
              key={p.key}
              title={p.title}
              desc={p.desc}
              checked={fleet[p.key]}
              onChange={(on) => onFleet({ ...fleet, [p.key]: on })}
            />
          ))}
        </div>
      </section>
      {showCap || showModes || showAnswerCap ? (
        <section className="fd-access-sec">
          <h4 className="fd-label fd-access-h">Limits</h4>
          <div className="fd-access-grid">
            {showCap ? (
              <Limit
                label="Session limit"
                value={fleet.spawnCap}
                min={1}
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
