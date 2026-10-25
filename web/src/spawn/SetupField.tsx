import { setupProblem } from './spawn';

type Props = {
  readonly value: string;
  readonly remember: boolean;
  readonly onValue: (v: string) => void;
  readonly onRemember: (on: boolean) => void;
};

/** DL-148: the optional setup command of a start, and the choice to remember it for the folder. */
export function SetupField({ value, remember, onValue, onRemember }: Props) {
  const problem = setupProblem(value);
  return (
    <div className="fd-spawn-setup-field">
      <label>
        Setup command (optional)
        <textarea
          rows={2}
          className="fd-spawn-setup"
          value={value}
          spellCheck={false}
          autoCapitalize="off"
          aria-invalid={problem === null ? undefined : true}
          onChange={(event) => onValue(event.target.value)}
        />
      </label>
      <p className="fd-note">
        Runs with /bin/sh in the new workspace before the agent starts. It has 10 minutes, and its
        background processes end with it.
      </p>
      {problem === null ? null : (
        <p id="fd-spawn-setup-error" role="alert">
          {problem}
        </p>
      )}
      <label className="fd-spawn-opt">
        <input
          type="checkbox"
          checked={remember}
          onChange={(event) => onRemember(event.target.checked)}
        />
        Remember this command for this folder
      </label>
    </div>
  );
}
