// R3.3 and R3.7 (DL-164): one project (`#/project/<id>`) with the tabs "Issues" and
// "Pull requests".
import { useState } from 'react';
import type { DeckSession, StationView } from '../gen';
import { stationName } from '../board/fleet';
import { useNow } from '../ui/useNow';
import { useIntegrations } from './context';
import { IssuesTab } from './IssuesTab';
import { ProjectChips } from './ProjectsPage';
import { PrsTab } from './PrsTab';

type Tab = 'issues' | 'prs';

type Props = {
  readonly id: string;
  readonly stations: readonly StationView[];
  readonly sessions: readonly DeckSession[];
  /** "Fix checks": opens the spawn form with the project's station and path. */
  readonly onFixChecks: (station: string, cwd: string, prompt: string) => void;
  /** Unix ms of "now" (tests). */
  readonly now?: number;
  readonly initialTab?: Tab;
};

export function ProjectPage({
  id,
  stations,
  sessions,
  onFixChecks,
  now: givenNow,
  initialTab = 'issues',
}: Props) {
  const env = useIntegrations();
  const now = useNow(givenNow, 30_000);
  const [tab, setTab] = useState<Tab>(initialTab);
  if (env === undefined) {
    return (
      <section className="fd-screen fd-int" aria-label="Project">
        <p className="fd-note">Projects need a live board.</p>
      </section>
    );
  }
  const project = env.projects?.find((p) => p.project_id === id);
  if (project === undefined) {
    return (
      <section className="fd-screen fd-int" aria-label="Project">
        <h1 className="fd-screen-title">Project</h1>
        {env.projects === null ? (
          env.error === null ? (
            env.actions.canAdmin ? (
              <p role="status" className="fd-note">
                Loading the project…
              </p>
            ) : (
              <p className="fd-note">Projects need the owner’s login.</p>
            )
          ) : (
            <p role="alert" className="fd-note fd-int-error">
              {env.error}
            </p>
          )
        ) : (
          <p className="fd-empty">This project does not exist.</p>
        )}
        <a href="#/projects">All projects</a>
      </section>
    );
  }
  return (
    <section className="fd-screen fd-int fd-int-project" aria-label={`Project ${project.name}`}>
      <h1 className="fd-screen-title">{project.name}</h1>
      <p className="fd-plan-meta">
        {stationName(project.station_id, stations)} · <code>{project.path}</code>
      </p>
      <ProjectChips project={project} />
      <div className="fd-pane-tabs" role="tablist" aria-label="Project views">
        <button
          type="button"
          role="tab"
          id="fd-int-tab-issues"
          aria-selected={tab === 'issues'}
          aria-controls="fd-int-panel"
          onClick={() => setTab('issues')}
        >
          Issues
        </button>
        <button
          type="button"
          role="tab"
          id="fd-int-tab-prs"
          aria-selected={tab === 'prs'}
          aria-controls="fd-int-panel"
          onClick={() => setTab('prs')}
        >
          Pull requests
        </button>
      </div>
      <div id="fd-int-panel" role="tabpanel" aria-labelledby={`fd-int-tab-${tab}`}>
        {tab === 'issues' ? (
          <IssuesTab project={project} actions={env.actions} stations={stations} now={now} />
        ) : (
          <PrsTab
            project={project}
            actions={env.actions}
            stations={stations}
            sessions={sessions}
            now={now}
            onFixChecks={onFixChecks}
          />
        )}
      </div>
    </section>
  );
}
