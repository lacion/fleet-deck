// DL-176: the groups "by project" of the sidebar and of the fleet board.
import type { DeckSession, Project } from '../gen';

/** The name of the last group: the sessions that no project holds. */
export const NO_PROJECT = 'No project';

/** The id of the group "No project". It is not a project id (those are ULIDs). */
export const NO_PROJECT_ID = '';

export type ProjectGroup = {
  /** The project id, or [`NO_PROJECT_ID`]. */
  readonly id: string;
  readonly name: string;
  readonly cards: readonly DeckSession[];
};

/**
 * One group for each project that holds a session, by name, then "No project" last. A card whose
 * `project_id` names no known project (the list is not loaded, or the project is gone) is in "No
 * project".
 */
export function projectGroups(
  sessions: readonly DeckSession[],
  projects: readonly Project[],
): ProjectGroup[] {
  const known = new Map(projects.map((p) => [p.project_id, p.name]));
  const idOf = (card: DeckSession): string =>
    card.project_id !== undefined && known.has(card.project_id) ? card.project_id : NO_PROJECT_ID;
  const ids = [...new Set(sessions.map(idOf))].sort((a, b) =>
    a === b
      ? 0
      : a === NO_PROJECT_ID
        ? 1
        : b === NO_PROJECT_ID
          ? -1
          : (known.get(a) ?? '').localeCompare(known.get(b) ?? '') || a.localeCompare(b),
  );
  return ids.map((id) => ({
    id,
    name: id === NO_PROJECT_ID ? NO_PROJECT : (known.get(id) ?? id),
    cards: sessions.filter((c) => idOf(c) === id),
  }));
}
