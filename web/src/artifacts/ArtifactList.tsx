import { relativeAge } from '../app/sessions';
import type { DeckArtifact } from '../gen';
import { artifactHref, KIND_LABEL } from './kinds';

/** Newest first, optionally one kind only. The deck sorts too; a list that grew locally stays right. */
export function visible(
  items: readonly DeckArtifact[],
  kind: DeckArtifact['kind'] | 'all',
): DeckArtifact[] {
  return items
    .filter((a) => kind === 'all' || a.kind === kind)
    .sort((a, b) => b.updated_at - a.updated_at);
}

type Props = {
  readonly items: readonly DeckArtifact[];
  /** Unix ms of "now". */
  readonly now: number;
  /** Names the station and the session on each row (the global page). */
  readonly where?: ((a: DeckArtifact) => string) | undefined;
  /** Opens the artifact in place; absent: each row links to the full page. */
  readonly onOpen?: ((a: DeckArtifact) => void) | undefined;
};

/** The artifact rows: title, kind, version and age. */
export function ArtifactList({ items, now, where, onOpen }: Props) {
  return (
    <ul className="fd-art-list" aria-label="Artifacts">
      {items.map((a) => {
        const content = (
          <>
            <span className="fd-art-title">{a.title}</span>
            <span className="fd-badge" data-kind={a.kind}>
              {KIND_LABEL[a.kind]}
            </span>
            <span className="fd-count">v{a.latest_version}</span>
            <span className="fd-art-age">{relativeAge(a.updated_at, now)}</span>
            {where === undefined ? null : <span className="fd-art-where">{where(a)}</span>}
          </>
        );
        return (
          <li key={`${a.station_id}/${a.artifact_id}`} data-artifact={a.artifact_id}>
            {onOpen === undefined ? (
              <a className="fd-art-row" href={artifactHref(a.station_id, a.artifact_id)}>
                {content}
              </a>
            ) : (
              <button type="button" className="fd-art-row" onClick={() => onOpen(a)}>
                {content}
              </button>
            )}
          </li>
        );
      })}
    </ul>
  );
}
