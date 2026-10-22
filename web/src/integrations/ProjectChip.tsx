// DL-176: the chip of a session that belongs to a project. It links to the project page. The
// project list is the owner's, so a login without it shows no chip.
import { createContext, type ReactNode, useContext } from 'react';
import type { DeckSession } from '../gen';
import { useProjects } from './context';

/** The project of the page around the strips: its own chip says nothing there. */
const Own = createContext<string | null>(null);

/** Inside it, the chip of project `id` is absent (the project page does not repeat its name). */
export function OwnProject({
  id,
  children,
}: {
  readonly id: string;
  readonly children: ReactNode;
}) {
  return <Own.Provider value={id}>{children}</Own.Provider>;
}

export function ProjectChip({ card }: { readonly card: DeckSession }) {
  const projects = useProjects();
  const own = useContext(Own);
  const project = projects.find((p) => p.project_id === card.project_id);
  if (project === undefined || project.project_id === own) return null;
  return (
    <a
      className="fd-badge fd-int-projectchip"
      data-project={project.project_id}
      title={`Project ${project.name}`}
      href={`#/project/${encodeURIComponent(project.project_id)}`}
    >
      {project.name}
    </a>
  );
}
