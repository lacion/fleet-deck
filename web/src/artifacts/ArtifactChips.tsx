// R12 (DL-165): the chips of the chat. "Artifact: <title> v<n>" opens the viewer; "Keep <file> as
// artifact" publishes a suggested file and then becomes the published chip.
import { useState } from 'react';
import type { ArtifactKind } from '../gen';
import { Icon, Paperclip } from '../ui/icons';
import { keptKey, useArtifacts } from './context';
import { artifactHref, fileName } from './kinds';

export type PublishedChip = {
  readonly artifact_id: string;
  readonly version: number;
  readonly title: string;
  readonly artifact_kind: ArtifactKind;
};

/** The text of the published chip. */
export const chipText = (chip: Pick<PublishedChip, 'title' | 'version'>): string =>
  `Artifact: ${chip.title} v${chip.version}`;

/** The chip of a published artifact; it opens the viewer. */
export function ArtifactChip({
  chip,
  fromEvent = false,
}: {
  readonly chip: PublishedChip;
  /** The chip of an `artifact.published` event: hidden when a "Keep" chip already shows it. */
  readonly fromEvent?: boolean;
}) {
  const env = useArtifacts();
  if (fromEvent && env?.kept?.has(keptKey(chip.artifact_id, chip.version)) === true) return null;
  const content = (
    <>
      <Icon of={Paperclip} size={12} /> {chipText(chip)}
    </>
  );
  if (env?.open !== undefined) {
    const open = env.open;
    return (
      <p className="fd-chat-line fd-art-chip-line" data-artifact={chip.artifact_id}>
        <button
          type="button"
          className="fd-chip fd-art-chip"
          onClick={() => open(chip.artifact_id, chip.version)}
        >
          {content}
        </button>
      </p>
    );
  }
  return (
    <p className="fd-chat-line fd-art-chip-line" data-artifact={chip.artifact_id}>
      {env?.session === undefined ? (
        <span className="fd-chip fd-art-chip">{content}</span>
      ) : (
        <a
          className="fd-chip fd-art-chip"
          href={artifactHref(env.session.station, chip.artifact_id, chip.version)}
        >
          {content}
        </a>
      )}
    </p>
  );
}

type Keep =
  | { readonly kind: 'idle' }
  | { readonly kind: 'busy' }
  | { readonly kind: 'error'; readonly text: string }
  | { readonly kind: 'done'; readonly chip: PublishedChip };

/** The chip of a suggested file: "Keep <file name> as artifact". Not shown without the drive scope. */
export function SuggestedChip({
  path,
  initial = { kind: 'idle' },
}: {
  readonly path: string;
  /** The state at the first render (tests). */
  readonly initial?: Keep;
}) {
  const env = useArtifacts();
  const [state, setState] = useState<Keep>(initial);
  if (state.kind === 'done') return <ArtifactChip chip={state.chip} />;
  if (env === undefined || !env.actions.canPublish || env.session === undefined) return null;
  const { actions, session } = env;
  const name = fileName(path);
  const keep = async () => {
    setState({ kind: 'busy' });
    try {
      const done = await actions.publish({
        station: session.station,
        session_id: session.session,
        path,
        title: name,
        artifact_id: null,
      });
      env.keep?.(keptKey(done.artifact.artifact_id, done.version.version));
      setState({
        kind: 'done',
        chip: {
          artifact_id: done.artifact.artifact_id,
          version: done.version.version,
          title: done.artifact.title,
          artifact_kind: done.artifact.kind,
        },
      });
    } catch (e) {
      setState({ kind: 'error', text: e instanceof Error ? e.message : 'The request failed.' });
    }
  };
  return (
    <p className="fd-chat-line fd-art-chip-line" data-suggested={path}>
      <button
        type="button"
        className="fd-chip fd-art-chip"
        disabled={state.kind === 'busy'}
        onClick={() => void keep()}
      >
        <Icon of={Paperclip} size={12} />{' '}
        {state.kind === 'busy' ? `Publishing ${name}…` : `Keep ${name} as artifact`}
      </button>
      {state.kind === 'error' ? (
        <span className="fd-art-error" role="alert">
          {' '}
          {state.text}
        </span>
      ) : null}
    </p>
  );
}
