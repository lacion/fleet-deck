// DL-176: the project of a session on the board.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { FleetHome } from '../app/FleetHome';
import { sidebarModel } from '../app/sessions';
import { Sidebar } from '../app/Sidebar';
import { FleetBoard } from '../board/FleetBoard';
import type { CardStatus, DeckSession } from '../gen';
import { sessionActions } from '../session/commands';
import { sessionFlags } from '../session/controller';
import { MoveToProject } from '../session/MoveToProject';
import { SessionHeader } from '../session/SessionHeader';
import { card, station } from '../testing/fleet';
import { integrationActions } from './actions';
import { IntegrationsProvider } from './context';
import { fakeActions, NOW, project } from './fixtures';
import { ProjectChip } from './ProjectChip';
import { NO_PROJECT, projectGroups } from './projectGroups';
import { ProjectPage } from './ProjectPage';

const noop = () => {};

const inProject = (
  id: string,
  projectId: string | undefined,
  status: CardStatus = 'working',
): DeckSession => ({
  ...card(id, status),
  ...(projectId === undefined ? {} : { project_id: projectId }),
});

const atlas = project();
const borealis = project({ project_id: 'p2', name: 'Borealis', path: '/work/borealis' });
const otherStation = project({ project_id: 'p3', name: 'Elsewhere', station_id: 'station-b' });

const provide = (node: React.ReactNode, projects = [atlas, borealis], actions = fakeActions()) =>
  renderToStaticMarkup(
    <IntegrationsProvider actions={actions} initial={projects}>
      {node}
    </IntegrationsProvider>,
  );

describe('the Sessions tab of a project', () => {
  const page = (sessions: readonly DeckSession[], extra = {}) =>
    provide(
      <ProjectPage
        id="p1"
        stations={[station()]}
        sessions={sessions}
        onFixChecks={noop}
        onNewSession={noop}
        now={NOW}
        {...extra}
      />,
    );

  test('it is the first tab and the page opens on it', () => {
    const html = page([]);
    const tabs = html.match(/role="tab"[^>]*>[^<]*</g)?.map((t) => t.replace(/.*>(.*)</, '$1'));
    expect(tabs).toEqual(['Sessions', 'Issues', 'Pull requests', 'Setup']);
    expect(html).toContain('id="fd-int-tab-sessions" aria-selected="true"');
  });

  test('an empty project says so and offers a new session', () => {
    const html = page([inProject('x', 'p2'), inProject('y', undefined)]);
    expect(html).toContain('No sessions in this project yet.');
    expect(html).toContain('New session in this project');
  });

  test('it lists the live and the ended sessions of the project only', () => {
    const html = page([
      inProject('live-1', 'p1'),
      inProject('gone-1', 'p1', 'ended'),
      inProject('other', 'p2'),
      inProject('none', undefined),
    ]);
    expect(html).toContain('data-session="live-1"');
    expect(html).toContain('data-session="gone-1"');
    expect(html).not.toContain('data-session="other"');
    expect(html).not.toContain('data-session="none"');
    expect(html).toContain('Live <span class="fd-count">1</span>');
    expect(html).toContain(
      '<details class="fd-lane-ended fd-int-ended"><summary>Ended <span class="fd-count">1</span></summary>',
    );
    expect(html).not.toContain('No sessions in this project yet.');
    expect(html).not.toContain('No live sessions in');
  });

  test('a project whose sessions all ended says that nothing runs, above the Ended fold', () => {
    const html = page([inProject('gone-1', 'p1', 'ended')]);
    expect(html).toContain('<p class="fd-note fd-int-none">No live sessions in Atlas.</p>');
    expect(html.indexOf('No live sessions in Atlas.')).toBeLessThan(html.indexOf('fd-int-ended'));
    expect(html).not.toContain('aria-label="Live sessions"');
  });

  test('the sessions are strips that name their station, without the chip of this project', () => {
    const html = page([inProject('live-1', 'p1')]);
    expect(html).toContain('class="fd-strip"');
    expect(html).toContain('data-showstation="true"');
    expect(html).not.toContain('class="fd-card"');
    expect(html).not.toContain('fd-int-projectchip');
    expect(html).not.toContain('fd-strip-proj');
  });

  test('without the drive scope there is no button', () => {
    const html = provide(
      <ProjectPage
        id="p1"
        stations={[station()]}
        sessions={[]}
        onFixChecks={noop}
        onNewSession={noop}
        now={NOW}
      />,
      [atlas],
      fakeActions({ canDrive: false }),
    );
    expect(html).not.toContain('New session in this project');
  });
});

describe('the project chip', () => {
  test('it links to the project page by name', () => {
    const html = provide(<ProjectChip card={inProject('a', 'p1')} />);
    expect(html).toContain('href="#/project/p1"');
    expect(html).toContain('</svg> Atlas</a>');
  });

  test('no project, or an unknown one, gives no chip', () => {
    expect(provide(<ProjectChip card={inProject('a', undefined)} />)).toBe('');
    expect(provide(<ProjectChip card={inProject('a', 'gone')} />)).toBe('');
  });

  test('a fleet card carries the chip', () => {
    const html = provide(
      <FleetBoard sessions={[inProject('a', 'p1')]} stations={[station()]} freshness="live" />,
    );
    expect(html).toContain('fd-int-projectchip');
  });
});

describe('grouping by project', () => {
  const cards = [
    inProject('a', 'p2'),
    inProject('b', 'p1'),
    inProject('c', undefined),
    inProject('d', 'p1'),
    inProject('e', 'gone'),
  ];

  test('groups are the projects by name, then "No project" last', () => {
    const groups = projectGroups(cards, [borealis, atlas]);
    expect(groups.map((g) => [g.name, g.cards.map((c) => c.summary.session_id)])).toEqual([
      ['Atlas', ['b', 'd']],
      ['Borealis', ['a']],
      [NO_PROJECT, ['c', 'e']],
    ]);
  });

  test('the sidebar model groups by project and keeps the station model without projects', () => {
    const stations = [station()];
    const byProject = sidebarModel(cards, stations, '', undefined, [atlas, borealis]);
    expect(byProject.groups.map((g) => [g.name, g.count])).toEqual([
      ['Atlas', 2],
      ['Borealis', 1],
      [NO_PROJECT, 2],
    ]);
    const byStation = sidebarModel(cards, stations, '');
    expect(byStation.groups.map((g) => g.name)).toEqual(['Alpha']);
  });

  test('the sidebar shows the choice only when projects exist', () => {
    const render = (projects: (typeof atlas)[]) =>
      provide(
        <Sidebar
          sessions={cards}
          stations={[station()]}
          route={{ name: 'home' }}
          terminalCount={0}
          onNavigate={noop}
          onOpenTerminals={noop}
        />,
        projects,
      );
    const withProjects = render([atlas]);
    expect(withProjects).toContain('aria-label="Group sessions by"');
    expect(withProjects).toContain('>Project<');
    expect(render([])).not.toContain('Group sessions by');
  });

  test('the fleet board makes one lane for each project, "No project" last', () => {
    const html = provide(
      <FleetBoard sessions={cards} stations={[station()]} freshness="live" group="project" />,
      [atlas, borealis],
    );
    const lanes = [...html.matchAll(/aria-label="Project ([^"]+)"/g)].map((m) => m[1]);
    expect(lanes).toEqual(['Atlas', 'Borealis', NO_PROJECT]);
  });

  test('the fleet home offers Group by project when projects exist', () => {
    const home = (projects: (typeof atlas)[]) =>
      provide(
        <FleetHome
          sessions={cards}
          stations={[station()]}
          freshness="live"
          assign={undefined}
          fleet={undefined}
          conflicts={undefined}
          feed={[]}
          layout="desktop"
          onOpen={noop}
          onSpawn={noop}
        />,
        projects,
      );
    expect(home([atlas])).toContain('>Group by project<');
    expect(home([])).not.toContain('>Group by project<');
  });
});

describe('Move to project', () => {
  const session = (projectId?: string): DeckSession => ({
    ...card('s1', 'working', 'station-a'),
    ...(projectId === undefined ? {} : { project_id: projectId }),
  });

  test('the select offers the projects of the station, No project and Automatic', () => {
    const html = provide(<MoveToProject session={session('p1')} onClose={noop} />, [
      atlas,
      borealis,
      otherStation,
    ]);
    const options = [...html.matchAll(/<option value="([^"]*)"[^>]*>([^<]*)</g)].map((m) => [
      m[1],
      m[2],
    ]);
    expect(options).toEqual([
      ['p1', 'Atlas'],
      ['p2', 'Borealis'],
      ['none', 'No project'],
      ['auto', 'Automatic (by folder)'],
    ]);
    expect(html).toContain('<option value="p1" selected');
    expect(html).toContain('aria-label="Move to project"');
  });

  test('a session without a project starts on Automatic', () => {
    const html = provide(<MoveToProject session={session()} onClose={noop} />);
    expect(html).toContain('<option value="auto" selected');
  });

  const header = (actions = fakeActions()) => {
    const s = session();
    const drive = sessionActions(async () => ({}), { station_id: 'station-a', session_id: 's1' }, [
      'read',
      'drive',
    ]);
    const idle = { flags: sessionFlags(s, 'live', drive) } as never;
    return provide(
      <SessionHeader
        session={s}
        stations={[station()]}
        controller={
          { outcome: null, busy: null, act: noop, dismiss: noop, ...(idle as object) } as never
        }
        actions={drive}
        terminal={null}
      />,
      [atlas],
      actions,
    );
  };

  test('the session menu has the item for a login with drive only', () => {
    expect(header()).toContain('Move to project…');
    expect(header(fakeActions({ canDrive: false }))).not.toContain('Move to project…');
  });
});

describe('the project calls', () => {
  test('they post to the three routes of the deck', async () => {
    const calls: [string, unknown][] = [];
    const io = {
      get: async () => ({}),
      post: async (path: string, body: unknown) => {
        calls.push([path, body]);
        return {};
      },
      put: async () => ({}),
      del: async () => null,
    };
    const actions = integrationActions(io, ['read', 'drive']);
    await actions.assignSession('p/1', 'st', 's 1');
    await actions.clearSessionProject('st', 's 1');
    await actions.autoSessionProject('st', 's 1');
    expect(calls).toEqual([
      ['/api/projects/p%2F1/sessions/assign', { station: 'st', session_id: 's 1' }],
      ['/api/sessions/st/s%201/project/clear', {}],
      ['/api/sessions/st/s%201/project/auto', {}],
    ]);
  });
});
