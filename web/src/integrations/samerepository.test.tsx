// FS-2 (design §2.1, §3.7): the "Same repository" panel and the merge call.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { ApiError } from '../runtime/api';
import { station } from '../testing/fleet';
import { integrationActions } from './actions';
import { IntegrationsProvider } from './context';
import { fakeActions, project } from './fixtures';
import { MergeDialogBody, SameRepository, sameRepository } from './SameRepository';

const dest = 'https://git.test:443/acme/atlas.git';
const atlas = project({ project_id: 'p1', name: 'Atlas', remote_dest: dest });
const copy = project({
  project_id: 'p2',
  name: 'Atlas copy',
  station_id: 'station-b',
  path: '/srv/atlas',
  remote_dest: dest,
});
const other = project({
  project_id: 'p3',
  name: 'Other',
  remote_dest: 'https://git.test:443/x.git',
});
const bare = project({ project_id: 'p4', name: 'Bare', remote_dest: null });
const stations = [station('station-a'), station('station-b')];

const panel = (projects: ReturnType<typeof project>[], canAdmin = true) =>
  renderToStaticMarkup(
    <IntegrationsProvider actions={fakeActions({ canAdmin })} initial={projects}>
      <SameRepository project={atlas} stations={stations} />
    </IntegrationsProvider>,
  );

describe('sameRepository', () => {
  test('it keeps the other projects with the same remote_dest, in order', () => {
    const third = project({ project_id: 'p5', name: 'Third', remote_dest: dest });
    expect(
      sameRepository(atlas, [atlas, other, copy, bare, third]).map((p) => p.project_id),
    ).toEqual(['p2', 'p5']);
  });

  test('a project without a remote_dest has no match, not even another without one', () => {
    expect(sameRepository(bare, [bare, project({ project_id: 'p6', remote_dest: null })])).toEqual(
      [],
    );
  });
});

describe('the Same repository panel', () => {
  test('it lists each other project with its station and path, and a Merge button', () => {
    const html = panel([atlas, copy, other]);
    expect(html).toContain('aria-label="Same repository"');
    expect(html).toContain(
      'These projects use the same repository. Merge them to work with one project on all stations.',
    );
    expect(html).toContain('Atlas copy · Beta · /srv/atlas');
    expect(html).toContain('aria-label="Merge Atlas copy into this project"');
    expect(html).toContain('Merge into this project');
    expect(html).not.toContain('Other');
  });

  test('it shows nothing without a match', () => {
    expect(panel([atlas, other, bare])).toBe('');
  });

  test('it shows nothing without the admin login', () => {
    expect(panel([atlas, copy], false)).toBe('');
  });
});

describe('the merge dialog', () => {
  const body = (extra: Partial<Parameters<typeof MergeDialogBody>[0]> = {}) =>
    renderToStaticMarkup(
      <MergeDialogBody
        from={copy}
        target={atlas}
        busy={false}
        error={null}
        onMerge={() => {}}
        onCancel={() => {}}
        {...extra}
      />,
    );

  test('it names the projects and has Cancel and Merge', () => {
    const html = body();
    expect(html).toContain(
      'The homes of Atlas copy become homes of Atlas, with their folders. Sessions and schedules move to Atlas. The settings, prompts, trackers and secret file list of Atlas copy are deleted.',
    );
    expect(html).toContain('>Cancel</button>');
    expect(html).toContain('>Merge</button>');
    expect(html).not.toContain('role="alert"');
  });

  test('while it runs the button says Merging… and is off', () => {
    const html = body({ busy: true });
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>Merging…<\/button>/);
  });

  test('an error shows in the dialog', () => {
    const html = body({ error: 'The projects use other repositories.' });
    expect(html).toContain('role="alert"');
    expect(html).toContain('The projects use other repositories.');
  });
});

describe('the merge call', () => {
  const rig = (reply: unknown = { project_id: 'p1', name: 'Atlas' }, fail?: ApiError) => {
    const calls: [string, string, unknown][] = [];
    const answer = async (method: string, path: string, body?: unknown) => {
      calls.push([method, path, body]);
      if (fail !== undefined) throw fail;
      return reply;
    };
    const io = {
      get: (path: string) => answer('GET', path),
      post: (path: string, body: unknown) => answer('POST', path, body),
      put: (path: string, body: unknown) => answer('PUT', path, body),
      del: (path: string) => answer('DELETE', path),
    };
    return { calls, actions: integrationActions(io, ['read', 'drive', 'admin']) };
  };

  test('it posts from and a command id to the merge route', async () => {
    const { calls, actions } = rig();
    expect((await actions.mergeProject('p 1', 'p2')).name).toBe('Atlas');
    expect(calls[0]?.[0]).toBe('POST');
    expect(calls[0]?.[1]).toBe('/api/projects/p%201/merge');
    expect(calls[0]?.[2]).toMatchObject({
      from: 'p2',
      command_id: expect.stringMatching(/^[0-9a-f]{32}$/),
    });
  });

  test('an invalid answer is refused and a refusal passes through', async () => {
    await expect(rig({}).actions.mergeProject('p1', 'p2')).rejects.toThrow('invalid project');
    const refused = rig({}, new ApiError(409, 'A session runs in a home.', 'home_busy'));
    await expect(refused.actions.mergeProject('p1', 'p2')).rejects.toMatchObject({
      status: 409,
      code: 'home_busy',
      message: 'A session runs in a home.',
    });
  });
});
