import { useState } from 'react';
import type { AssignActions, AssignResult } from './assign';

type Props = {
  readonly actions: AssignActions | undefined;
  /** Opens the spawn form for an unrouted task. */
  readonly onSpawn: (stationId: string) => void;
};

/** `assign auto: <task>` on the fleet screen (M3 6-3, DL-204). */
export function AssignBar({ actions, onSpawn }: Props) {
  const [task, setTask] = useState('');
  const [repo, setRepo] = useState('');
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<AssignResult | null>(null);
  if (actions === undefined) return null;
  return (
    <form
      className="fd-assign"
      aria-label="Assign a task"
      onSubmit={(event) => {
        event.preventDefault();
        if (task.trim() === '' || busy) return;
        setBusy(true);
        void actions
          .assign(task.trim(), repo)
          .then((r) => {
            setResult(r);
            if (r.ok) setTask('');
          })
          .finally(() => setBusy(false));
      }}
    >
      <label>
        Task
        <input
          value={task}
          onChange={(event) => setTask(event.target.value)}
          placeholder="Give a task to the best session"
        />
      </label>
      <label>
        Repository
        <input
          value={repo}
          onChange={(event) => setRepo(event.target.value)}
          placeholder="Any repository"
        />
      </label>
      <button type="submit" disabled={busy || task.trim() === ''}>
        Assign auto
      </button>
      {result === null ? null : (
        <p role="status">
          {result.text}{' '}
          {result.spawnOn === null ? null : (
            <button type="button" onClick={() => onSpawn(result.spawnOn?.stationId ?? '')}>
              Spawn on {result.spawnOn.name}
            </button>
          )}
        </p>
      )}
    </form>
  );
}
