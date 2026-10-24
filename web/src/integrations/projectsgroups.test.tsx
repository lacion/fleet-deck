// FS-2 (design §3.7): the note on the projects page for projects that share one repository.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { station } from '../testing/fleet';
import { IntegrationsProvider } from './context';
import { fakeActions, project } from './fixtures';
import { ProjectsPage, sameRepoGroups } from './ProjectsPage';

const dest = 'https://git.test:443/acme/atlas.git';
const a = project({ project_id: 'a', name: 'A', remote_dest: dest, repo: 'git.test/acme/atlas' });
const b = project({ project_id: 'b', name: 'B', remote_dest: dest, repo: null });
const c = project({ project_id: 'c', name: 'C', remote_dest: 'https://h:443/x.git', repo: null });
const d = project({ project_id: 'd', name: 'D', remote_dest: 'https://h:443/x.git', repo: null });
const e = project({ project_id: 'e', name: 'E', remote_dest: null });
const f = project({ project_id: 'f', name: 'F', remote_dest: null });
const g = project({ project_id: 'g', name: 'G', remote_dest: 'https://h:443/one.git' });

describe('sameRepoGroups', () => {
  test('it groups two or more projects with one remote_dest, in the order of the first one', () => {
    expect(sameRepoGroups([c, a, g, b, e, d, f])).toEqual([
      { repo: 'https://h:443/x.git', count: 2 },
      { repo: 'git.test/acme/atlas', count: 2 },
    ]);
  });

  test('it skips null remote_dest and single projects', () => {
    expect(sameRepoGroups([e, f, g])).toEqual([]);
    expect(sameRepoGroups([])).toEqual([]);
  });

  test('the repo of the first project names the group, else the remote_dest', () => {
    expect(sameRepoGroups([b, a])).toEqual([{ repo: dest, count: 2 }]);
  });
});

describe('the projects page', () => {
  const page = (projects: ReturnType<typeof project>[]) =>
    renderToStaticMarkup(
      <IntegrationsProvider actions={fakeActions()} initial={projects}>
        <ProjectsPage stations={[station()]} />
      </IntegrationsProvider>,
    );

  test('it shows one note for each group', () => {
    const html = page([a, b, g]);
    expect(html).toContain('role="note"');
    expect(html).toContain('2 projects use git.test/acme/atlas. Open one of them to merge them.');
    expect(html.match(/role="note"/g)).toHaveLength(1);
  });

  test('it shows no note when no two projects share a repository', () => {
    expect(page([a, c, e])).not.toContain('role="note"');
  });
});
