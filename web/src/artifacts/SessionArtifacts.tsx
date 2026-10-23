import { useState } from 'react';
import type { SessionKey } from '../gen';
import { Loading } from '../ui/Loading';
import { ErrorNotice } from '../ui/page';
import { useNow } from '../ui/useNow';
import type { ArtifactActions } from './actions';
import { ArtifactList, visible } from './ArtifactList';
import { ArtifactViewer } from './ArtifactViewer';
import { type ListState, useArtifactList } from './useArtifactList';

/** One state of the session's list: loading, error, empty or the rows. */
export function SessionArtifactsList({
  state,
  now,
  onOpen,
  onRetry,
}: {
  readonly state: ListState;
  readonly now: number;
  readonly onOpen: (id: string) => void;
  readonly onRetry: () => void;
}) {
  if (state.kind === 'loading') return <Loading text="Loading the artifacts…" />;
  if (state.kind === 'error') return <ErrorNotice text={state.text} onRetry={onRetry} />;
  if (state.items.length === 0) {
    return (
      <p className="fd-note">
        This session has no artifacts yet. Publish a file from the Files tab, or ask the session to
        publish one.
      </p>
    );
  }
  return (
    <ArtifactList
      items={visible(state.items, 'all')}
      now={now}
      onOpen={(a) => onOpen(a.artifact_id)}
    />
  );
}

/** The session's Artifacts tab: its artifacts, and the viewer of the one that is open. */
export function SessionArtifacts({
  actions,
  session,
  count,
  open,
  onOpen,
  onClose,
}: {
  readonly actions: ArtifactActions;
  readonly session: SessionKey;
  /** `summary.artifacts_count`: the list reads again when it changes. */
  readonly count: number;
  readonly open: { readonly id: string; readonly version: number | null } | null;
  readonly onOpen: (id: string, version: number | null) => void;
  readonly onClose: () => void;
}) {
  const now = useNow(undefined, 30_000);
  const [retry, setRetry] = useState(0);
  const state = useArtifactList(
    actions,
    { station: session.station_id, session: session.session_id },
    count,
    retry,
  );
  if (open !== null) {
    return (
      <ArtifactViewer
        key={`${session.station_id}/${open.id}`}
        actions={actions}
        station={session.station_id}
        id={open.id}
        version={open.version}
        variant="panel"
        onVersion={(version) => onOpen(open.id, version)}
        onBack={onClose}
      />
    );
  }
  return (
    <section className="fd-art" aria-label="Artifacts">
      <h3 className="fd-vh">Artifacts</h3>
      <SessionArtifactsList
        state={state}
        now={now}
        onOpen={(id) => onOpen(id, null)}
        onRetry={() => setRetry((n) => n + 1)}
      />
    </section>
  );
}
