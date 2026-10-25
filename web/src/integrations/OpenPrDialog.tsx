// R3.9 (DL-164): "Open a pull request" from a session. The architect wires it into the session
// header; the dialog needs the project, the session and the integration calls from the context.
import { useState } from 'react';
import { sessionTitle } from '../app/sessions';
import type { DeckSession, Project } from '../gen';
import { useIntegrations } from './context';
import { systemLabel } from './model';
import { WriteDialog } from './WriteDialog';

/** The base branch when the board does not know the default of the forge. */
export const DEFAULT_BASE = 'main';

type Props = {
  readonly project: Project;
  readonly session: DeckSession;
  readonly onClose: () => void;
};

export function OpenPrDialog({ project, session, onClose }: Props) {
  const env = useIntegrations();
  const head = session.summary.branch ?? '';
  const [base, setBase] = useState(DEFAULT_BASE);
  const [title, setTitle] = useState(sessionTitle(session));
  const [draft, setDraft] = useState(false);
  const forge = project.forge;
  const ready =
    env !== undefined && forge !== null && head !== '' && base.trim() !== '' && title.trim() !== '';
  const system = forge === null ? 'the forge' : systemLabel(forge.kind);
  return (
    <WriteDialog
      title="Open a pull request"
      question={
        ready
          ? `Create ${draft ? 'this draft pull request' : 'this pull request'} on ${system} as you?`
          : null
      }
      preview={
        <pre className="fd-int-quote">
          {`${head} → ${base.trim()}\n${title.trim()}${draft ? '\n(draft)' : ''}`}
        </pre>
      }
      write={(id) => {
        if (env === undefined) return Promise.reject(new Error('The deck is not connected.'));
        return env.actions.openPr(project.project_id, {
          command_id: id,
          confirm: true,
          head,
          base: base.trim(),
          title: title.trim(),
          body: '',
          draft,
        });
      }}
      onClose={onClose}
    >
      {head === '' ? (
        <p className="fd-note" role="alert">
          This session has no branch.
        </p>
      ) : (
        <p className="fd-note">
          Branch <code>{head}</code> of {project.name}
        </p>
      )}
      {forge === null ? (
        <p className="fd-note" role="alert">
          This project has no forge binding.
        </p>
      ) : null}
      <label>
        Into branch
        <input value={base} autoComplete="off" onChange={(e) => setBase(e.target.value)} />
      </label>
      <label>
        Title
        <input value={title} maxLength={256} onChange={(e) => setTitle(e.target.value)} />
      </label>
      <label className="fd-int-check">
        <input type="checkbox" checked={draft} onChange={(e) => setDraft(e.target.checked)} />
        Draft
      </label>
    </WriteDialog>
  );
}
