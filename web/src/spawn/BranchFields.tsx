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
        <label>
          Branch
          <input
            value={branch}
            placeholder="fd/<callsign>, new"
            spellCheck={false}
            autoCapitalize="off"
            onChange={(event) => onBranch(event.target.value)}
          />
          {branchError === null ? null : <span role="alert">{sentence(branchError)}</span>}
        </label>
        {name === '' ? null : (
          <label>
            Base
            <input
              value={base}
              placeholder="HEAD"
              spellCheck={false}
              autoCapitalize="off"
              onChange={(event) => onBase(event.target.value)}
            />
            {baseError === null ? null : <span role="alert">{sentence(baseError)}</span>}
          </label>
        )}
      </div>
      <p className="fd-note">
        Empty: a new branch fd/&lt;callsign&gt;. An existing branch is checked out. A ticket key
        such as PROJ-123 becomes part of the callsign.
      </p>
    </div>
  );
}
