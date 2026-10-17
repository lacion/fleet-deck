import { baseProblem, branchProblem } from './spawn';

type Props = {
  readonly branch: string;
  readonly base: string;
  readonly onBranch: (value: string) => void;
  readonly onBase: (value: string) => void;
};

const sentence = (problem: string): string =>
  `${problem.charAt(0).toUpperCase()}${problem.slice(1)}.`;

/** DL-144: the branch and base inputs of a new worktree start. A base needs a branch. */
export function BranchFields({ branch, base, onBranch, onBase }: Props) {
  const name = branch.trim();
  const from = base.trim();
  const branchError = name === '' ? null : branchProblem(name);
  const baseError = name === '' || from === '' ? null : baseProblem(from);
  return (
    <div className="fd-spawn-branch">
      <div className="fd-spawn-branch-row">
        <div className="fd-spawn-branch-field">
          <label>
            Branch
            <input
              value={branch}
              placeholder="fd/<callsign>, new"
              spellCheck={false}
              autoCapitalize="off"
              aria-invalid={branchError === null ? undefined : true}
              aria-describedby={branchError === null ? undefined : 'fd-spawn-branch-error'}
              onChange={(event) => onBranch(event.target.value)}
            />
          </label>
          {branchError === null ? null : (
            <span id="fd-spawn-branch-error" role="alert">
              {sentence(branchError)}
            </span>
          )}
        </div>
        {name === '' ? null : (
          <div className="fd-spawn-branch-field">
            <label>
              Base
              <input
                value={base}
                placeholder="HEAD"
                spellCheck={false}
                autoCapitalize="off"
                aria-invalid={baseError === null ? undefined : true}
                aria-describedby={baseError === null ? undefined : 'fd-spawn-base-error'}
                onChange={(event) => onBase(event.target.value)}
              />
            </label>
            {baseError === null ? null : (
              <span id="fd-spawn-base-error" role="alert">
                {sentence(baseError)}
              </span>
            )}
          </div>
        )}
      </div>
      <p className="fd-note">
        Empty: a new branch fd/&lt;callsign&gt;. An existing branch is checked out. A ticket key
        such as PROJ-123 becomes part of the callsign.
      </p>
    </div>
  );
}
