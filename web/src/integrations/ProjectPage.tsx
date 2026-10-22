// R3.3 and R3.7 (DL-164): one project (`#/project/<id>`) with the tabs "Sessions" (DL-176),
// "Issues", "Pull requests" and "Setup" (the homes, the secret files and the lands of the project).
import { useState } from 'react';
import { usePhoneTitle } from '../app/Layout';
import type { DeckSession, StationView } from '../gen';
import { stationName } from '../board/fleet';
import { PathTail } from '../chat/PathTail';
import { homeShort } from '../ui/format';
import { Page } from '../ui/page';
import { useNow } from '../ui/useNow';
import { useIntegrations } from './context';
import { IssuesTab } from './IssuesTab';
import { ProjectChips } from './ProjectsPage';
import { ProjectHomes } from './ProjectHomes';
import { ProjectLands } from './ProjectLands';
import { SameRepository } from './SameRepository';
import { SecretFiles } from './SecretFiles';
import { PrsTab } from './PrsTab';
import { ProjectSessions } from './ProjectSessions';
import type { Freshness } from '../runtime/state';
import type { ListWorkspaces } from '../ui/FolderPicker';

type Tab = 'sessions' | 'issues' | 'prs' | 'setup';

type Props = {
  readonly id: string;
  readonly stations: readonly StationView[];
  readonly sessions: readonly DeckSession[];
  /** "Fix checks": opens the spawn form with the project's station and path. */
  readonly onFixChecks: (station: string, cwd: string, prompt: string) => void;
  /** DL-176: "New session in this project": the station, the project path (worktree on). */
  readonly onNewSession?: ((station: string, cwd: string) => void) | undefined;
  readonly freshness?: Freshness | undefined;
  /** Unix ms of "now" (tests). */
  readonly now?: number;
  readonly initialTab?: Tab;
  /** Reads the folders of a station, for "Browse…" in "Add home". */
  readonly list?: ListWorkspaces | undefined;
};

export function ProjectPage({
  id,
  stations,
  sessions,
  onFixChecks,
  onNewSession,
  freshness = 'live',
  now: givenNow,
  initialTab = 'sessions',
  list,
}: Props) {
  const env = useIntegrations();
  const now = useNow(givenNow, 30_000);
  const [tab, setTab] = useState<Tab>(initialTab);
  const project = env?.projects?.find((p) => p.project_id === id);
  // §4.8: the phone top bar names the project ("Project" while it loads).
  usePhoneTitle(project?.name);
  if (env === undefined) {
    return (
      <Page title="Project">
        <p className="fd-note">Projects need a live board.</p>
      </Page>
    );
  }
  if (project === undefined) {
    return (
      <Page title="Project">
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
      </Page>
    );
  }
  // The header says where the project lives (§4.3: the summary under the title); the forge and the
  // trackers are the header chips. The sessions come first under it; the configuration of the
  // project (Same repository, Homes, Secret files, Lands) is the Setup tab.
  const where = (
    <span className="fd-int-where">
      <span>{stationName(project.station_id, stations)}</span>
      <span className="fd-int-sep" aria-hidden="true">
        ·
      </span>
      <code className="fd-int-path" title={project.path}>
        <PathTail path={homeShort(project.path)} />
      </code>
    </span>
  );
  const chips =
    project.forge === null && project.trackers.length === 0 ? null : (
      <ProjectChips project={project} />
    );
  return (
    <Page
      title={project.name}
      back={{ href: '#/projects', label: 'All projects' }}
      label={`Project ${project.name}`}
      description={where}
      filters={chips}
      wide
      className="fd-int fd-int-project"
    >
      <div className="fd-pane-tabs" role="tablist" aria-label="Project views">
        <button
          type="button"
          role="tab"
          id="fd-int-tab-sessions"
          aria-selected={tab === 'sessions'}
          aria-controls="fd-int-panel"
          onClick={() => setTab('sessions')}
        >
          Sessions
        </button>
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
        <button
          type="button"
          role="tab"
          id="fd-int-tab-setup"
          aria-selected={tab === 'setup'}
          aria-controls="fd-int-panel"
          onClick={() => setTab('setup')}
        >
          Setup
        </button>
      </div>
      <div id="fd-int-panel" role="tabpanel" aria-labelledby={`fd-int-tab-${tab}`}>
        {tab === 'sessions' ? (
          <ProjectSessions
            project={project}
            sessions={sessions}
            stations={stations}
            freshness={freshness}
            now={now}
            onNewSession={onNewSession ?? (() => {})}
            canDrive={env.actions.canDrive && onNewSession !== undefined}
          />
        ) : tab === 'issues' ? (
          <IssuesTab project={project} actions={env.actions} stations={stations} now={now} />
        ) : tab === 'setup' ? (
          <div className="fd-int-tab fd-int-setup" data-tab="setup">
            <SameRepository project={project} stations={stations} />
            <ProjectHomes project={project} stations={stations} list={list} />
            <SecretFiles project={project} stations={stations} />
            <ProjectLands project={project} stations={stations} sessions={sessions} />
          </div>
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
    </Page>
  );
}
