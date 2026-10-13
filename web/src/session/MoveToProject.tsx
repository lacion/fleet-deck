// DL-176: "Move to project…" of the session menu. The owner chooses a project of the session's
// station, "No project" (the folder no longer matters), or "Automatic (by folder)".
import { useState } from 'react';
import type { DeckSession } from '../gen';
import { useIntegrations } from '../integrations/context';
import { Dialog } from '../integrations/Dialog';
import { message } from '../integrations/useLoad';
import { Icon, X } from '../ui/icons';

export const NO_PROJECT_CHOICE = 'none';
export const AUTO_CHOICE = 'auto';

type Props = {
  readonly session: DeckSession;
  readonly onClose: () => void;
};

export function MoveToProject({ session, onClose }: Props) {
  const env = useIntegrations();
  const projects = (env?.projects ?? []).filter((p) => p.station_id === session.station_id);
  const [choice, setChoice] = useState(
    session.project_id !== undefined && projects.some((p) => p.project_id === session.project_id)
      ? session.project_id
      : AUTO_CHOICE,
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const move = async () => {
    if (env === undefined) return;
    const { station_id: station } = session;
    const id = session.summary.session_id;
    setBusy(true);
    setError(null);
    try {
      if (choice === AUTO_CHOICE) await env.actions.autoSessionProject(station, id);
      else if (choice === NO_PROJECT_CHOICE) await env.actions.clearSessionProject(station, id);
      else await env.actions.assignSession(choice, station, id);
      onClose();
    } catch (e) {
      setError(message(e));
      setBusy(false);
    }
  };
  return (
    <Dialog title="Move to project" onClose={onClose}>
      <form
        className="fd-int-form"
        onSubmit={(e) => {
          e.preventDefault();
          void move();
        }}
      >
        <label>
          Project
          <select value={choice} onChange={(e) => setChoice(e.target.value)}>
            {projects.map((p) => (
              <option key={p.project_id} value={p.project_id}>
                {p.name}
              </option>
            ))}
            <option value={NO_PROJECT_CHOICE}>No project</option>
            <option value={AUTO_CHOICE}>Automatic (by folder)</option>
          </select>
        </label>
        {error === null ? null : (
          <p className="fd-note fd-int-error" role="alert">
            {error}
          </p>
        )}
        <div className="fd-pane-row">
          <button type="submit" className="fd-primary" disabled={busy}>
            {busy ? 'Moving…' : 'Move'}
          </button>
          <button type="button" className="fd-ghost" disabled={busy} onClick={onClose}>
            <Icon of={X} size={14} /> Cancel
          </button>
        </div>
      </form>
    </Dialog>
  );
}
