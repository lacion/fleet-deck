// The count of the session's Artifacts tab: the rows that its list shows. The list folds the older
// copies of a title under one row (`groupCopies`), so `artifacts_count` (16 artifacts) did not
// match the list (7 rows).
import { useEffect, useState } from 'react';
import type { ArtifactActions } from '../artifacts/actions';
import { groupCopies } from '../artifacts/ArtifactList';
import { artifactKey, useHiddenArtifacts } from '../artifacts/DeleteArtifact';
import type { DeckArtifact } from '../gen';

/** The rows of a list of artifacts: one per title, without the artifacts that the owner deleted. */
export function artifactRows(
  items: readonly DeckArtifact[],
  hidden: ReadonlySet<string> = new Set(),
): number {
  return groupCopies(items.filter((a) => !hidden.has(artifactKey(a.station_id, a.artifact_id))))
    .length;
}

/**
 * The rows of the session's artifact list; it reads the list again when `count` (the session's
 * `artifacts_count`) changes. `null` while the first read runs (no count is shown then); a failed
 * read gives `count`. No artifacts: 0, with no read.
 */
export function useArtifactRows(
  actions: ArtifactActions | undefined,
  station: string,
  session: string,
  count: number,
): number | null {
  const key = `${station}/${session}`;
  const [read, setRead] = useState<{
    readonly key: string;
    readonly items: readonly DeckArtifact[] | null;
  } | null>(null);
  useEffect(() => {
    if (actions === undefined || count === 0) return undefined;
    let live = true;
    actions.list({ station, session, q: '' }).then(
      (items) => {
        if (live) setRead({ key, items });
      },
      () => {
        if (live) setRead({ key, items: null });
      },
    );
    return () => {
      live = false;
    };
  }, [actions, station, session, key, count]);
  const hidden = useHiddenArtifacts();
  if (count === 0) return 0;
  if (read === null || read.key !== key) return null;
  return read.items === null ? count : artifactRows(read.items, hidden);
}
