// FG-5 (design §8.8): the Homes section, and the calls of the homes and of the remote URL.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { ProjectHome, StationView } from '../gen';
import { ApiError } from '../runtime/api';
import { station } from '../testing/fleet';
import { integrationActions } from './actions';
import { IntegrationsProvider } from './context';
import { fakeActions, project } from './fixtures';
import {
  AddHomeDialog,
  homeCandidates,
  homeFolderName,
  homeText,
  ProjectHomes,
} from './ProjectHomes';

const home = (extra: Partial<ProjectHome> = {}): ProjectHome => ({
  station_id: 'station-b',
  generation: 1,
  position: 1,
  state: 'ready',
  path: '/srv/atlas',
  reserved_path: null,
  attempt_id: null,
  sync_owner: false,
  code: null,
  message: null,
  ...extra,
});

const stations: StationView[] = [
  station('station-a'),
  station('station-b', {
    info: {
      fleet_git: {
        clone_root: '/srv/projects',
        allow_check_cmd: false,
        allow_local_remotes: false,
      },
    } as StationView['info'],
  }),
];

const section = (extra: Parameters<typeof project>[0] = {}, canAdmin = true) =>
  renderToStaticMarkup(
    <IntegrationsProvider actions={fakeActions({ canAdmin })} initial={[project(extra)]}>
      <ProjectHomes project={project(extra)} stations={stations} />
    </IntegrationsProvider>,
  );

describe('the text of a home row', () => {
  test('each state has the text of the design, with the station first', () => {
    expect(homeText(home(), 'Bravo')).toBe('Bravo · /srv/atlas · Ready');
    expect(
      homeText(home({ state: 'cloning', path: null, reserved_path: '/srv/p/atlas' }), 'Bravo'),
    ).toBe('Bravo · Cloning into /srv/p/atlas…');
    expect(homeText(home({ state: 'unverified', message: 'No remote origin.' }), 'Bravo')).toBe(
      'Bravo · Not verified: No remote origin.',
    );
    expect(homeText(home({ state: 'failed', code: 'clone_failed' }), 'Bravo')).toBe(
      'Bravo · Failed: clone_failed',
    );
    expect(homeText(home({ state: 'failed' }), 'Bravo')).toBe('Bravo · Failed: no reason given');
    expect(homeText(home({ state: 'unknown' }), 'Bravo')).toBe('Bravo · State unknown');
  });

  test('the folder name follows the station rule', () => {
    expect(homeFolderName('My App/2')).toBe('My-App-2');
    expect(homeFolderName('.hidden')).toBe('-hidden');
    expect(homeFolderName('a'.repeat(90))).toHaveLength(80);
  });

  test('a station can get a home when it reports Fleet Git and has none', () => {
    const p = project({ homes: [home({ station_id: 'station-a' })] });
    expect(homeCandidates(p, stations).map((s) => s.station_id)).toEqual(['station-b']);
    expect(homeCandidates(project({ homes: [home()] }), stations)).toEqual([]);
  });
});

describe('the Homes section', () => {
  const two = {
    homes: [
      home({ station_id: 'station-a', position: 0, path: '/work/atlas' }),
      home({ state: 'failed', message: 'The clone failed: no space.' }),
    ],
    remote_url: 'https://git.test/acme/atlas.git',
    repo: 'git.test/acme/atlas',
  };

  test('it lists the homes, with Retry for a failed one and Remove on each of two', () => {
    const html = section(two);
    expect(html).toContain('aria-label="Homes"');
    expect(html).toContain('Alpha · /work/atlas · Ready');
    expect(html).toContain('Beta · Failed: The clone failed: no space.');
    expect(html).toContain('aria-label="Retry the home on Beta"');
    expect(html).not.toContain('aria-label="Retry the home on Alpha"');
    expect(html).toContain('aria-label="Remove the home on Alpha"');
    expect(html).toContain('aria-label="Remove the home on Beta"');
    expect(html).toContain('Add home');
  });

  test('the last home has no Remove button', () => {
    const html = section();
    expect(html).toContain('Homes');
    expect(html).not.toContain('Remove the home');
  });

  test('without a remote URL Add home is off and says why', () => {
    const html = section();
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>.*Add home/s);
    expect(html).toContain('Set the remote URL of the project first.');
  });

  test('it lists the Git names', () => {
    const html = section({
      ...two,
      slugs: [
        { slug: 'alpha', station_id: 'station-a' },
        { slug: 'bravo', station_id: 'station-b' },
      ],
    });
    expect(html).toContain('Git names: alpha → Alpha, bravo → Beta');
  });

  test('a login without the admin scope sees the rows and no button', () => {
    const html = section(two, false);
    expect(html).toContain('Beta · Failed');
    expect(html).not.toContain('Retry');
    expect(html).not.toContain('Remove');
    expect(html).not.toContain('Add home');
  });

  test('an older deck sends no homes: nothing shows and nothing breaks', () => {
    const old = { ...project(), homes: undefined } as unknown as ReturnType<typeof project>;
    const html = renderToStaticMarkup(
      <IntegrationsProvider actions={fakeActions()} initial={[old]}>
        <ProjectHomes project={old} stations={stations} />
      </IntegrationsProvider>,
    );
    expect(html).toBe('');
    expect(section({ homes: [] })).toBe('');
  });
});

describe('the Add home dialog', () => {
  const dialog = (initialChoice: 'clone' | 'adopt') => {
    const p = project({
      remote_url: 'https://h.example/acme/atlas.git',
      repo: 'h.example/acme/atlas',
    });
    return renderToStaticMarkup(
      <AddHomeDialog
        project={p}
        candidates={homeCandidates({ homes: [] }, stations)}
        actions={fakeActions()}
        onDone={() => undefined}
        onClose={() => undefined}
        initialChoice={initialChoice}
      />,
    );
  };

  test('it offers two choices and clones a new copy by default', () => {
    const html = dialog('clone');
    expect(html).toContain('Clone a new copy');
    expect(html).toContain('Use an existing folder on that station');
    expect(html).toMatch(/<input[^>]*type="radio"[^>]*checked=""[^>]*\/>\s*Clone a new copy/);
    expect(html).toContain(
      'The station clones https://h.example/acme/atlas.git into /srv/projects/',
    );
    expect(html).not.toContain('Folder path');
  });

  test('the existing folder choice asks for a path and keeps Add home off until it is absolute', () => {
    const html = dialog('adopt');
    expect(html).toContain('Folder path');
    expect(html).toContain('placeholder="/home/you/projects/app"');
    expect(html).toContain(
      'The station checks that this folder is a complete clone of h.example/acme/atlas. It never clones into it.',
    );
    expect(html).not.toContain('The station clones');
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>Add home/);
  });

  test('the existing folder has Browse… when the board can read the station folders', () => {
    const p = project({ remote_url: 'https://h.example/acme/atlas.git' });
    const render = (list?: () => Promise<never>) =>
      renderToStaticMarkup(
        <AddHomeDialog
          project={p}
          candidates={homeCandidates({ homes: [] }, stations)}
          actions={fakeActions()}
          list={list}
          onDone={() => undefined}
          onClose={() => undefined}
          initialChoice="adopt"
        />,
      );
    expect(render(() => new Promise<never>(() => {}))).toContain('Browse…');
    expect(render()).not.toContain('Browse…');
  });
});

describe('the calls', () => {
  const rig = (reply: unknown = {}, fail?: ApiError) => {
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

  test('add, retry and remove use the home routes, with a command id', async () => {
    const { calls, actions } = rig({ generation: 2, attempt_id: 'ha-1' });
    expect(await actions.addHome('p 1', 'st-b')).toEqual({ generation: 2, attempt_id: 'ha-1' });
    await actions.retryHome('p1', 'st-b');
    await actions.removeHome('p1', 'st-b');
    expect(calls.map(([m, p]) => `${m} ${p}`)).toEqual([
      'POST /api/projects/p%201/homes',
      'POST /api/projects/p1/homes/st-b/retry',
      'DELETE /api/projects/p1/homes/st-b',
    ]);
    const add = calls[0]?.[2] as { station: string; command_id: string };
    expect(add.station).toBe('st-b');
    expect(add.command_id).toMatch(/^[0-9a-f]{32}$/);
    expect(calls[1]?.[2]).toMatchObject({ command_id: expect.stringMatching(/^[0-9a-f]{32}$/) });
  });

  test('addHome sends the path only when it is given', async () => {
    const { calls, actions } = rig({ generation: 2, attempt_id: 'ha-1' });
    await actions.addHome('p1', 'st-b');
    await actions.addHome('p1', 'st-b', '/home/you/projects/app');
    expect(Object.keys(calls[0]?.[2] as object).sort()).toEqual(['command_id', 'station']);
    expect(calls[1]?.[2]).toMatchObject({ station: 'st-b', path: '/home/you/projects/app' });
  });

  test('a refusal of a home is thrown with its message', async () => {
    const { actions } = rig({}, new ApiError(409, 'A session runs in this home.', 'home_busy'));
    await expect(actions.removeHome('p1', 'st-b')).rejects.toThrow('A session runs in this home.');
  });

  test('the remote route sends the URL, the rebind flag and a command id', async () => {
    const answer = {
      remote_url: 'https://h/b.git',
      remote_ssh_user: null,
      repo: 'h/b',
      remote_dest: 'https://h:443/b.git',
      git_revision: 4,
    };
    const { calls, actions } = rig(answer);
    expect(await actions.setRemote('p1', 'https://h/b.git', true)).toEqual({
      kind: 'saved',
      answer,
    });
    expect(calls[0]?.[0]).toBe('PUT');
    expect(calls[0]?.[1]).toBe('/api/projects/p1/remote');
    expect(calls[0]?.[2]).toMatchObject({ remote_url: 'https://h/b.git', rebind: true });
  });

  test('the remote route maps rebind_needed and invalid_git_settings', async () => {
    const needed = rig(
      {},
      new ApiError(409, 'This URL names another repository.', 'rebind_needed'),
    );
    expect(await needed.actions.setRemote('p1', 'https://h/b.git', false)).toEqual({
      kind: 'rebind_needed',
      message: 'This URL names another repository.',
    });
    const bad = rig({}, new ApiError(400, 'No password.', 'invalid_git_settings', 'remote_url'));
    expect(await bad.actions.setRemote('p1', 'x', false)).toEqual({
      kind: 'invalid',
      message: 'No password.',
    });
    const other = rig({}, new ApiError(403, 'owner only', 'owner_only'));
    await expect(other.actions.setRemote('p1', 'x', false)).rejects.toThrow('owner only');
  });

  test('a share refusal of the Git save names the artifact_share field', async () => {
    const { actions } = rig(
      {},
      new ApiError(400, 'This share exposes ~/.ssh.', 'share_exposed', 'artifact_share'),
    );
    expect(await actions.saveGit('p1', { settings: project().git, revision: 1 })).toEqual({
      kind: 'invalid',
      field: 'artifact_share',
      message: 'This share exposes ~/.ssh.',
    });
  });

  test('placement and the config reference use their routes', async () => {
    const placed = rig({ stations: ['st-b', 'st-a'] });
    expect(
      await placed.actions.place({
        count: 2,
        requires: ['gpu'],
        prefer: ['idle'],
        spread: true,
        provider: 'claude',
      }),
    ).toEqual(['st-b', 'st-a']);
    expect(placed.calls[0]?.[1]).toBe('/api/placement');
    const prefs = rig({ drift_reference: 'st-a' });
    expect(await prefs.actions.fleetGitPrefs()).toEqual({ drift_reference: 'st-a' });
    expect(await prefs.actions.setDriftReference('st-b')).toEqual({ drift_reference: 'st-a' });
    expect(prefs.calls.map(([m, p, b]) => `${m} ${p} ${JSON.stringify(b ?? null)}`)).toEqual([
      'GET /api/fleet-git/prefs null',
      'PUT /api/fleet-git/drift-reference {"station":"st-b"}',
    ]);
    const bad = rig({ stations: 'no' });
    await expect(
      bad.actions.place({ count: 1, requires: [], prefer: [], spread: false }),
    ).rejects.toThrow('invalid placement answer');
  });
});
