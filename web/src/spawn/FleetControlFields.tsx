import { answerCapProblem, type FleetControl, fleetOn, MAX_ANSWER_CAP } from './fleetControl';

type Props = {
  readonly fleet: FleetControl;
  readonly onChange: (next: FleetControl) => void;
};

const SWITCHES: readonly {
  readonly key: 'scope' | 'inspect' | 'manage' | 'answer';
  readonly text: string;
}[] = [
  {
    key: 'scope',
    text: 'Fleet scope: reach every session in the fleet, not only the sessions it starts',
  },
  { key: 'inspect', text: 'Inspect: read diffs, files, artifacts and integrations' },
  {
    key: 'manage',
    text: 'Manage: resume, rename, set the mode, move to a project, start in a project',
  },
  {
    key: 'answer',
    text: 'Answer approvals: allow once or deny approvals, and answer questions',
  },
];

/** ADR-008 §3: the fleet-control switches under the Coordinator checkbox (owner only). */
export function FleetControlFields({ fleet, onChange }: Props) {
  const problem = answerCapProblem(fleet);
  return (
    <fieldset className="fd-spawn-fleet">
      <legend>Fleet control</legend>
      {SWITCHES.map((s) => (
        <label key={s.key} className="fd-spawn-opt">
          <input
            type="checkbox"
            checked={fleet[s.key]}
            onChange={(event) => onChange({ ...fleet, [s.key]: event.target.checked })}
          />
          {s.text}
        </label>
      ))}
      {fleet.answer ? (
        <>
          <label>
            Answer limit
            <input
              type="number"
              inputMode="numeric"
              min={0}
              max={MAX_ANSWER_CAP}
              value={fleet.answerCap}
              onChange={(event) => onChange({ ...fleet, answerCap: event.target.value })}
            />
          </label>
          {problem === null ? null : (
            <p className="fd-note" role="alert">
              {problem}
            </p>
          )}
        </>
      ) : null}
      {fleetOn(fleet) ? (
        <p className="fd-note">
          Fleet control asks you to confirm it is you. It works only while the deck setting Fleet
          control is on, and on stations that allow it.
        </p>
      ) : null}
    </fieldset>
  );
}
