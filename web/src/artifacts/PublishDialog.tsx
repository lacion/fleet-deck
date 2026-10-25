import { useEffect, useRef, useState } from 'react';
import { trapTab, useFocusReturn } from '../app/focus';
import { useOverlay } from '../app/overlay';
import type { ArtifactPublished } from '../gen';
import { Icon, X } from '../ui/icons';
import { fileName } from './kinds';
import { useArtifacts } from './context';

const message = (e: unknown) => (e instanceof Error ? e.message : 'The request failed.');

/** The text of a finished publish: a new version, or "no change" when the bytes are the same. */
export function publishedText(done: ArtifactPublished): string {
  const { title } = done.artifact;
  return done.created
    ? `Published ${title} as version ${done.version.version}.`
    : `${title} has not changed: it stays at version ${done.version.version}.`;
}

type Props = {
  /** The file, relative to the session directory. */
  readonly path: string;
  readonly onClose: () => void;
  readonly onPublished: (done: ArtifactPublished) => void;
  /** An error text, so that a static render shows it (tests). */
  readonly initialError?: string;
};

/** "Publish as artifact": asks for a title (default: the file name) and publishes the file. */
export function PublishDialog({ path, onClose, onPublished, initialError }: Props) {
  const env = useArtifacts();
  const root = useRef<HTMLDivElement>(null);
  const field = useRef<HTMLInputElement>(null);
  useFocusReturn(root, field);
  useOverlay();
  const [title, setTitle] = useState(fileName(path));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(initialError ?? null);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const session = env?.session;
  const publish = async () => {
    if (env === undefined || session === undefined) return;
    setBusy(true);
    setError(null);
    try {
      const done = await env.actions.publish({
        station: session.station,
        session_id: session.session,
        path,
        title: title.trim() === '' ? null : title.trim(),
        artifact_id: null,
      });
      if (alive.current) onPublished(done);
    } catch (e) {
      if (alive.current) setError(message(e));
    } finally {
      if (alive.current) setBusy(false);
    }
  };
  return (
    // The overlay click is a mouse shortcut; the Cancel button and Escape serve the keyboard.
    <div
      className="fd-overlay"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        ref={root}
        className="fd-dialog fd-publish"
        role="dialog"
        aria-modal="true"
        aria-label="Publish as artifact"
        onKeyDown={(e) => {
          if (e.key === 'Escape') {
            e.stopPropagation();
            onClose();
          } else trapTab(e, root.current);
        }}
      >
        <h2>Publish as artifact</h2>
        <form
          className="fd-share-form"
          onSubmit={(e) => {
            e.preventDefault();
            void publish();
          }}
        >
          <p className="fd-note">
            <code>{path}</code>
          </p>
          <label>
            Title
            <input
              ref={field}
              value={title}
              maxLength={200}
              onChange={(e) => setTitle(e.target.value)}
            />
          </label>
          {error === null ? null : (
            <p className="fd-note fd-share-error" role="alert">
              {error}
            </p>
          )}
          <div className="fd-pane-row">
            <button type="submit" className="fd-primary" disabled={busy}>
              {busy ? 'Publishing…' : 'Publish'}
            </button>
            <button type="button" className="fd-ghost" onClick={onClose}>
              <Icon of={X} size={14} /> Cancel
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
