// FG-7 (design §8.6, §10.5, §10.6): the radar panel grouped by project.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { FileConflict } from '../gen';
import type { RadarAnswer, RadarGroup } from '../integrations/actions';
import { IntegrationsProvider } from '../integrations/context';
import { fakeActions, project } from '../integrations/fixtures';
import { ConflictRadar } from './ConflictRadar';
import { hasProjectGroups, RadarGroups, radarHasRows } from './RadarGroups';

const NOW = 1_760_000_000_000;

const same: FileConflict = {
  repo: 'github.com/o/app',
  file: 'src/lib.rs',
  touches: [
    {
      station_id: 'st-b',
      session_id: 's2',
      session_title: 'Beta fix',
      path: 'lib.rs',
      touched_at: NOW - 60_000,
    },
    { station_id: 'st-a', session_id: 's1', session_title: null, path: 'lib.rs', touched_at: 1 },
  ],
};

const group = (extra: Partial<RadarGroup> = {}): RadarGroup => ({
  project_id: 'p1',
  group_key: 'origin:github.com/o/app',
  will_conflict: [
    {
      a: { station_id: 'st-a', session_id: 's1', callsign: 'ada' },
      b: { station_id: 'st-b', session_id: 's2', callsign: 'bob' },
      files: ['a.rs', 'b.rs', 'c.rs', 'd.rs', 'e.rs'],
      checked_at: NOW - 3 * 60_000,
    },
  ],
  same_files: [],
  ...extra,
});

const render = (radar: RadarAnswer, projects = [project({ name: 'Atlas' })]) =>
  renderToStaticMarkup(
    <RadarGroups radar={radar} projects={projects} onOpen={() => {}} now={NOW} />,
  );

describe('the grouped radar', () => {
  test('a group has a header, and a Will conflict row with files, names and age', () => {
    const html = render({ groups: [group()], notices: [] });
    expect(html).toContain('>Atlas</li>');
    expect(html).toContain('data-tone="hazard">Will conflict</span>');
    expect(html).toContain('a.rs, b.rs, c.rs');
    expect(html).toContain('+2');
    expect(html).toContain('>ada</button>');
    expect(html).toContain('>bob</button>');
    expect(html).toContain('3 min ago');
  });

  test('the default branch is named by the remote and the default of the project', () => {
    const p = project({
      name: 'Atlas',
      git: { ...project().git, push_remote: 'origin', default_branch: 'main' },
    });
    const g = group({
      will_conflict: [
        {
          a: { station_id: 'st-a', session_id: 's1', title: 'Fix login' },
          b: 'default',
          files: ['a.rs'],
          checked_at: NOW,
        },
      ],
    });
    const html = render({ groups: [g], notices: [] }, [p]);
    expect(html).toContain('>Fix login</button>');
    expect(html).toContain('<span>origin/main</span>');
    expect(html).toContain('just now');
  });

  test('a group without a project is "No project"; an unknown project shows its id', () => {
    const none = render({ groups: [group({ project_id: null })], notices: [] });
    expect(none).toContain('>No project</li>');
    expect(render({ groups: [group({ project_id: 'zz' })], notices: [] })).toContain('>zz</li>');
  });

  test('the same-files rows of a group keep the DL-306 layout', () => {
    const html = render({
      groups: [group({ will_conflict: [], same_files: [same] })],
      notices: [],
    });
    expect(html).toContain('aria-label="Conflict src/lib.rs"');
    expect(html).toContain('>Conflict</span>');
    expect(html).toContain('>Beta fix</button>');
    expect(html).toContain('1 min ago');
  });

  test('a group shows 3 rows of each kind and "+n more"', () => {
    const many = Array.from({ length: 5 }, (_, i) => ({
      ...same,
      file: `f${i}.rs`,
    }));
    const html = render({ groups: [group({ will_conflict: [], same_files: many })], notices: [] });
    expect(html).toContain('f2.rs');
    expect(html).not.toContain('f3.rs');
    expect(html).toContain('+2 more');
  });

  test('the notices of a project are footer lines, once; a project with only a notice has a group', () => {
    const notice = { project_id: 'p1', code: 'station_offline', text: 'Radar: Alpha is offline.' };
    const two = render({ groups: [group(), group({ group_key: 'root:x' })], notices: [notice] });
    expect(two.split('Radar: Alpha is offline.')).toHaveLength(2);
    const only = render({
      groups: [group({ project_id: null })],
      notices: [{ ...notice, project_id: 'p1' }],
    });
    expect(only).toContain('>Atlas</li>');
    expect(only).toContain('Radar: Alpha is offline.');
  });
});

describe('when the panel groups', () => {
  test('only when some group has a project and something to show', () => {
    expect(hasProjectGroups(null)).toBe(false);
    expect(hasProjectGroups({ groups: [group({ project_id: null })], notices: [] })).toBe(false);
    expect(hasProjectGroups({ groups: [group()], notices: [] })).toBe(true);
    expect(radarHasRows({ groups: [group({ will_conflict: [] })], notices: [] })).toBe(false);
    expect(radarHasRows({ groups: [group()], notices: [] })).toBe(true);
  });

  test('the panel looks as before while the radar has not answered or the deck is older', () => {
    const html = renderToStaticMarkup(
      <IntegrationsProvider actions={fakeActions()} initial={[]}>
        <ConflictRadar list={[same]} problem={null} stations={[]} onOpen={() => {}} />
      </IntegrationsProvider>,
    );
    expect(html).toContain('aria-label="Conflict src/lib.rs"');
    expect(html).not.toContain('Will conflict');
    const bare = renderToStaticMarkup(
      <ConflictRadar list={null} problem={null} stations={[]} onOpen={() => {}} />,
    );
    expect(bare).toBe('');
  });
});
