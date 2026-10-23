import type { ReactNode } from 'react';
import { relativeAge } from '../app/sessions';
import type { ArtifactKind, DeckArtifact } from '../gen';
import { Code, FileText, Icon, ImageIcon, type LucideIcon, Play } from '../ui/icons';
import { artifactHref, KIND_LABEL } from './kinds';

/** The icon of each artifact kind, before the title. */
const KIND_ICON: Record<ArtifactKind, LucideIcon> = {
  html: Code,
  markdown: FileText,
  image: ImageIcon,
  svg: ImageIcon,
  pdf: FileText,
  video: Play,
  other: FileText,
};

/** Newest first, optionally one kind only. The deck sorts too; a list that grew locally stays right. */
export function visible(
  items: readonly DeckArtifact[],
  kind: DeckArtifact['kind'] | 'all',
): DeckArtifact[] {
  return items
    .filter((a) => kind === 'all' || a.kind === kind)
    .sort((a, b) => b.updated_at - a.updated_at);
}

/** One row of the list: the newest artifact of a title, and its older copies. */
export type ArtifactGroup = {
  readonly newest: DeckArtifact;
  /** The older artifacts of the same station, session and title, newest first. */
  readonly older: readonly DeckArtifact[];
};

/**
 * The artifacts of one station, session and title in one group (a session that publishes a file
 * again makes a new artifact): the newest is the row, the others are its older copies. The groups
 * keep the order of their newest artifact.
 */
export function groupCopies(items: readonly DeckArtifact[]): ArtifactGroup[] {
  const groups = new Map<string, DeckArtifact[]>();
  for (const a of [...items].sort((x, y) => y.updated_at - x.updated_at)) {
    const key = JSON.stringify([a.station_id, a.session_id, a.title]);
    groups.set(key, [...(groups.get(key) ?? []), a]);
  }
  return [...groups.values()].map(([newest, ...older]) => ({
    newest: newest as DeckArtifact,
    older,
  }));
}

/** The exact time of an older copy: "21 Oct, 01:12". */
export const exactTime = (ms: number): string =>
  new Date(ms).toLocaleString('en-GB', {
    day: 'numeric',
    month: 'short',
    hour: '2-digit',
    minute: '2-digit',
  });

type Props = {
  readonly items: readonly DeckArtifact[];
  /** Unix ms of "now". */
  readonly now: number;
  /** Names the station and the session on each row (the global page). */
  readonly where?: ((a: DeckArtifact) => string) | undefined;
  /** Opens the artifact in place; absent: each row links to the full page. */
  readonly onOpen?: ((a: DeckArtifact) => void) | undefined;
};

/** A row: a link to the full page, or a button that opens the artifact in place. */
function RowLink({
  artifact,
  className,
  onOpen,
  children,
}: {
  readonly artifact: DeckArtifact;
  readonly className: string;
  readonly onOpen: Props['onOpen'];
  readonly children: ReactNode;
}) {
  return onOpen === undefined ? (
    <a className={className} href={artifactHref(artifact.station_id, artifact.artifact_id)}>
      {children}
    </a>
  ) : (
    <button type="button" className={className} onClick={() => onOpen(artifact)}>
      {children}
    </button>
  );
}

/**
 * The artifact rows: the kind icon, the title (with a version badge after the first), where it
 * came from, the kind and the age. The older copies of a title fold under its row: the fold is
 * the one notation of the copies (a count after the title read as a version). The kind is a quiet
 * word that the icon says too: the icon carries its name for a screen reader, and a phone shows
 * only the icon (artifacts.css).
 */
export function ArtifactList({ items, now, where, onOpen }: Props) {
  return (
    <ul className="fd-art-list" aria-label="Artifacts">
      {groupCopies(items).map(({ newest: a, older }) => (
        <li key={`${a.station_id}/${a.artifact_id}`} data-artifact={a.artifact_id}>
          <RowLink artifact={a} className="fd-art-row" onOpen={onOpen}>
            <span className="fd-art-kind">
              <Icon of={KIND_ICON[a.kind]} size={14} />
              <span className="fd-vh">{KIND_LABEL[a.kind]}: </span>
            </span>
            {/* The fold under the row says how many older copies there are: no count here. A
                version after the first is a neutral badge, apart from the words of the title. */}
            <span className="fd-art-titlecell">
              <span className="fd-art-title">{a.title}</span>
              {a.latest_version > 1 ? (
                <span className="fd-badge fd-art-ver">v{a.latest_version}</span>
              ) : null}
            </span>
            <span className="fd-badge" data-kind={a.kind} data-tone="muted" aria-hidden="true">
              {KIND_LABEL[a.kind]}
            </span>
            <span className="fd-art-age">{relativeAge(a.updated_at, now)}</span>
            {where === undefined ? null : <span className="fd-art-where">{where(a)}</span>}
          </RowLink>
          {older.length === 0 ? null : (
            <details className="fd-art-copies">
              <summary>
                {older.length === 1 ? '1 older copy' : `${older.length} older copies`}
              </summary>
              <ul aria-label={`Older copies of ${a.title}`}>
                {older.map((o) => (
                  <li key={`${o.station_id}/${o.artifact_id}`} data-artifact={o.artifact_id}>
                    <RowLink artifact={o} className="fd-art-copy" onOpen={onOpen}>
                      <span className="fd-vh">{o.title}, </span>
                      <time dateTime={new Date(o.updated_at).toISOString()}>
                        {exactTime(o.updated_at)}
                      </time>
                      {o.latest_version > 1 ? (
                        <span className="fd-badge fd-art-ver">v{o.latest_version}</span>
                      ) : null}
                      <span className="fd-art-age">{relativeAge(o.updated_at, now)}</span>
                    </RowLink>
                  </li>
                ))}
              </ul>
            </details>
          )}
        </li>
      ))}
    </ul>
  );
}
