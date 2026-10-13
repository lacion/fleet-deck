// DL-176: the chip of a session that belongs to a project. It links to the project page. The
// project list is the owner's, so a login without it shows no chip.
import type { DeckSession } from '../gen';
import { useProjects } from './context';

export function ProjectChip({ card }: { readonly card: DeckSession }) {
  const projects = useProjects();
  const project = projects.find((p) => p.project_id === card.project_id);
  if (project === undefined) return null;
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
