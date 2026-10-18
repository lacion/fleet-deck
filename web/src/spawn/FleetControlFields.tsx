import {
  answerCapProblem,
  captureCapProblem,
  desktopCapProblem,
  type FleetControl,
  fleetOn,
  imageBudgetProblem,
  MAX_ANSWER_CAP,
} from './fleetControl';

type Props = {
  readonly fleet: FleetControl;
  readonly onChange: (next: FleetControl) => void;
};

const SWITCHES: readonly {
  readonly key: 'scope' | 'inspect' | 'manage' | 'answer' | 'desktop' | 'capture';
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
  {
    key: 'desktop',
    text: 'Desktop: show notifications and previews on grant stations, close its own previews, and show its work on walls',
  },
  {
    key: 'capture',
    text: 'Screenshots: capture a whole monitor of a grant station and read its own screenshots',
  },
];

const DESKTOP_NOTE =
  'Opening a preview runs web content as your user on that station, and the page can make network requests.';
const CAPTURE_NOTE =
  'This agent can capture the whole selected monitor, including other applications, notifications and private information visible there. Screenshots are stored as artifacts. Images the agent reads are sent to its provider conversation. Deleting an artifact does not remove downloaded copies or provider conversations.';
const DESKTOP_GATE_NOTE =
  'Desktop and screenshot tools work only while the deck settings Fleet control and Desktop control are on, and on stations that allow them.';

type LimitProps = {
  readonly label: string;
  readonly value: string;
  readonly max: number;
  readonly problem: string | null;
  readonly onChange: (value: string) => void;
};

export function LimitInput({ label, value, max, problem, onChange }: LimitProps) {
  return (
    <>
      <label>
        {label}
        <input
          type="number"
          inputMode="numeric"
          min={0}
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
    </>
  );
}

/** ADR-008 §3: the fleet-control switches under the Coordinator checkbox (owner only). */
export function FleetControlFields({ fleet, onChange }: Props) {
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
        <LimitInput
          label="Answer limit"
          value={fleet.answerCap}
          max={MAX_ANSWER_CAP}
          problem={answerCapProblem(fleet)}
          onChange={(answerCap) => onChange({ ...fleet, answerCap })}
        />
      ) : null}
      {fleet.desktop || fleet.capture ? (
        <LimitInput
          label="Desktop action limit"
          value={fleet.desktopCap}
          max={MAX_ANSWER_CAP}
          problem={desktopCapProblem(fleet)}
          onChange={(desktopCap) => onChange({ ...fleet, desktopCap })}
        />
      ) : null}
      {fleet.capture ? (
        <>
          <LimitInput
            label="Screenshot limit"
            value={fleet.captureCap}
            max={MAX_ANSWER_CAP}
            problem={captureCapProblem(fleet)}
            onChange={(captureCap) => onChange({ ...fleet, captureCap })}
          />
          <LimitInput
            label="Image read budget (MiB)"
            value={fleet.imageMib}
            max={1024}
            problem={imageBudgetProblem(fleet)}
            onChange={(imageMib) => onChange({ ...fleet, imageMib })}
          />
        </>
      ) : null}
      {fleet.desktop ? <p className="fd-note">{DESKTOP_NOTE}</p> : null}
      {fleet.capture ? <p className="fd-note">{CAPTURE_NOTE}</p> : null}
      {fleet.desktop || fleet.capture ? <p className="fd-note">{DESKTOP_GATE_NOTE}</p> : null}
      {fleetOn(fleet) ? (
        <p className="fd-note">
          Fleet control asks you to confirm it is you. It works only while the deck setting Fleet
          control is on, and on stations that allow it.
        </p>
      ) : null}
    </fieldset>
  );
}
