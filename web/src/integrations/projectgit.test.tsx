// FG-1..FG-4 (design §17.1): the Git sync section of the project form.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { ApiError } from '../runtime/api';
import { integrationActions } from './actions';
import { IntegrationsProvider } from './context';
import { fakeActions, project } from './fixtures';
import { emptyDraft } from './model';
import { ProjectForm } from './ProjectForm';
import {
  buildGitPut,
  foreignMachine,
  type GitBase,
  gitDraftOf,
  ProjectGitSettings,
  remoteChange,
} from './ProjectGitSettings';

const base = (): GitBase => ({ settings: project().git, revision: 3 });

const section = (extra: Parameters<typeof project>[0] = {}, canAdmin = true) =>
  renderToStaticMarkup(
    <IntegrationsProvider actions={fakeActions({ canAdmin })} initial={[project(extra)]}>
      <ProjectGitSettings projectId="p1" />
    </IntegrationsProvider>,
  );

describe('the Git sync section', () => {
  test('it shows the labels, options and help lines of the design', () => {
    const html = section();
    for (const text of [
      'Git sync',
      'Sync this project across stations',
      'Branches get the station name. Fleet Deck fetches before each new worktree.',
      'Push branches',
      'After each turn',
      'When the session ends',
      'Fleet Deck pushes only fd/&lt;station&gt;/… branches that it made.',
      'Remote',
      'WIP remote URL (optional)',
      'Pushes go here instead of the remote, for example a bare repository on an always-on server.',
      'Default branch (optional)',
      'Empty: the branch that the remote names.',
      'Clean up merged worktrees',
      'When merged',
      'Moves the worktree to the trash. It never deletes a branch.',
      'After (hours)',
      'Copy ignored files into new worktrees',
      'For example .env. Each file at most 1 MiB.',
      'Environment for agents and setup',
      'PATH, HOME, LD_*, DYLD_* and provider keys are not allowed.',
      'Remote URL',
      'Filled from the origin of the first home. Check it. Fleet Deck compares the exact URL form (user@host:path and ssh://user@host/path are different).',
      'Artifact share (optional)',
      'A Taildrive share as &lt;machine&gt;/&lt;share&gt;.',
      'Save Git settings',
    ]) {
      expect(html).toContain(text);
    }
    expect(html).toContain('placeholder="origin"');
    // The settings of the land and the radar (later phases) are not on the form.
    expect(html).not.toContain('Land as');
    expect(html).not.toContain('Radar station');
    expect(html).toContain('placeholder="nas/artifacts"');
    expect(html).not.toContain('Check command');
  });

  test('the hours field is disabled while cleanup is off', () => {
    expect(section()).toMatch(/<input type="number"[^>]*disabled=""/);
    const on = section({ git: { ...project().git, cleanup: 'merged' } });
    expect(on).not.toMatch(/<input type="number"[^>]*disabled=""/);
  });

  test('the section shows only for a login with the admin scope', () => {
    expect(section({}, false)).toBe('');
    expect(renderToStaticMarkup(<ProjectGitSettings projectId="p1" />)).toBe('');
  });

  test('the form shows it only when editing an existing project', () => {
    const render = (editing: boolean, projectId: string | undefined) =>
      renderToStaticMarkup(
        <IntegrationsProvider actions={fakeActions()} initial={[project()]}>
          <ProjectForm
            stations={[]}
            draft={emptyDraft('station-a')}
            onChange={() => {}}
            editing={editing}
            projectId={projectId}
            saving={false}
            error={null}
            accounts={null}
            onSave={() => {}}
            onCancel={() => {}}
          />
        </IntegrationsProvider>,
      );
    expect(render(true, 'p1')).toContain('Save Git settings');
    expect(render(false, undefined)).not.toContain('Save Git settings');
    expect(render(true, 'p1')).toContain('>Save</button>');
  });

  test('the draft shows the loaded settings, one path and one NAME=value per line', () => {
    const draft = gitDraftOf(
      project({
        remote_url: 'https://git.test/acme/atlas.git',
        git: {
          ...project().git,
          sync: true,
          push: 'on_turn',
          wip_remote: 'ssh://bare.test/wip.git',
          worktree_include: ['.env', 'config/*.local'],
          worktree_env: { RUSTC_WRAPPER: 'sccache', A: 'b=c' },
        },
      }),
    );
    expect(draft.include).toBe('.env\nconfig/*.local');
    expect(draft.env).toBe('RUSTC_WRAPPER=sccache\nA=b=c');
    expect(draft.remoteUrl).toBe('https://git.test/acme/atlas.git');
    expect(draft.wipRemote).toBe('ssh://bare.test/wip.git');
    expect(draft.afterHours).toBe('24');
  });
});

describe('the save body', () => {
  test('it sends the ten fields and the three of later phases unchanged', () => {
    const loaded = {
      ...project().git,
      land: 'merge' as const,
      check_cmd: 'make check',
      artifact_share: 'nas/artifacts',
      radar_home: 'station-a',
    };
    const draft = gitDraftOf(project({ git: loaded }));
    const built = buildGitPut(
      {
        ...draft,
        sync: true,
        push: 'on_exit',
        pushRemote: '  ',
        wipRemote: ' ',
        defaultBranch: '',
        cleanup: 'merged',
        afterHours: '48',
        include: ' .env \n\n config/*.local \n',
        env: 'RUSTC_WRAPPER = sccache\n\nA=b=c',
      },
      { settings: loaded, revision: 3 },
    );
    expect(built).toEqual({
      put: {
        revision: 3,
        settings: {
          ...loaded,
          sync: true,
          push: 'on_exit',
          push_remote: 'origin',
          wip_remote: null,
          default_branch: null,
          cleanup: 'merged',
          cleanup_after_hours: 48,
          worktree_include: ['.env', 'config/*.local'],
          worktree_env: { RUSTC_WRAPPER: 'sccache', A: 'b=c' },
        },
      },
    });
  });

  test('the remote URL goes to its own route, only when it changed', () => {
    const draft = gitDraftOf(project({ remote_url: 'https://h/a.git' }));
    expect(remoteChange(draft, 'https://h/a.git')).toBeNull();
    expect(remoteChange({ ...draft, remoteUrl: '' }, null)).toBeNull();
    expect(remoteChange({ ...draft, remoteUrl: ' https://h/b.git ' }, 'https://h/a.git')).toBe(
      'https://h/b.git',
    );
    // The settings PUT never carries the URL: the remote route stores it first.
    const built = buildGitPut({ ...draft, remoteUrl: 'https://h/b.git' }, base());
    expect('put' in built && 'remote_url' in built.put).toBe(false);
  });

  test('the board refuses the simple rules before it calls the deck', () => {
    const draft = gitDraftOf(project());
    expect(buildGitPut({ ...draft, push: 'on_turn' }, base())).toEqual({
      problems: { push: 'Turn on sync first.' },
    });
    expect(buildGitPut({ ...draft, cleanup: 'merged' }, base())).toEqual({
      problems: { cleanup: 'Turn on sync first.' },
    });
    const on = { ...draft, sync: true, cleanup: 'merged' as const };
    for (const hours of ['0', '721', '', 'x', '1.5']) {
      expect(buildGitPut({ ...on, afterHours: hours }, base())).toEqual({
        problems: { cleanup_after_hours: 'Clean up after 1 to 720 hours.' },
      });
    }
    for (const hours of ['1', '720']) {
      expect('put' in buildGitPut({ ...on, afterHours: hours }, base())).toBe(true);
    }
    expect(buildGitPut({ ...draft, env: 'PATH' }, base())).toEqual({
      problems: { worktree_env: 'Use NAME=value.' },
    });
    expect(buildGitPut({ ...draft, env: '=x' }, base())).toEqual({
      problems: { worktree_env: 'Use NAME=value.' },
    });
  });

  test('a disabled hours field keeps the loaded value', () => {
    const draft = { ...gitDraftOf(project()), afterHours: 'junk' };
    const loaded = { ...project().git, cleanup_after_hours: 36 };
    const built = buildGitPut(draft, { settings: loaded, revision: 1 });
    expect('put' in built && built.put.settings.cleanup_after_hours).toBe(36);
  });
});

describe('the save call', () => {
  const rig = (fail?: ApiError) => {
    const calls: [string, unknown][] = [];
    const io = {
      get: async () => ({}),
      post: async () => ({}),
      put: async (path: string, body: unknown) => {
        calls.push([path, body]);
        if (fail !== undefined) throw fail;
        return { git: project().git, git_revision: 4 };
      },
      del: async () => null,
    };
    return { calls, actions: integrationActions(io, ['read', 'drive', 'admin']) };
  };
  const body = { settings: project().git, revision: 3 };

  test('it puts to /api/projects/{id}/git and returns the answer', async () => {
    const { calls, actions } = rig();
    const result = await actions.saveGit('p 1', body);
    expect(calls).toEqual([['/api/projects/p%201/git', body]]);
    expect(result).toEqual({ kind: 'saved', answer: { git: project().git, git_revision: 4 } });
  });

  test('a 400 invalid_git_settings names its field', async () => {
    const { actions } = rig(
      new ApiError(400, 'Turn on sync first.', 'invalid_git_settings', 'push'),
    );
    expect(await actions.saveGit('p1', body)).toEqual({
      kind: 'invalid',
      field: 'push',
      message: 'Turn on sync first.',
    });
  });

  test('a 409 is a stale revision or a rebind, by its code', async () => {
    const stale = rig(
      new ApiError(409, 'The Git settings changed. Reload and try again.', 'stale_revision'),
    );
    expect((await stale.actions.saveGit('p1', body)).kind).toBe('stale');
    const rebind = rig(
      new ApiError(409, 'Turn off sync to change the remote URL.', 'rebind_required'),
    );
    expect(await rebind.actions.saveGit('p1', body)).toEqual({
      kind: 'rebind',
      message: 'Turn off sync to change the remote URL.',
    });
  });

  test('another failure is thrown', async () => {
    const { actions } = rig(new ApiError(403, 'owner only', 'owner_only'));
    await expect(actions.saveGit('p1', body)).rejects.toThrow('owner only');
  });
});

describe('the share that no station exports (FG-12)', () => {
  const exporting = (machine: string) => ({ info: { taildrive: { machine } } }) as never;

  test('the hint shows for a machine that no reporting station has', () => {
    const stations = [exporting('Alpha'), { info: null } as never];
    expect(foreignMachine('nas/artifacts', stations)).toBe('nas');
    expect(foreignMachine('alpha/out', stations)).toBeNull();
    expect(foreignMachine('ALPHA/out', stations)).toBeNull();
  });

  test('no hint without a slash, a machine, or a station that reports Taildrive', () => {
    const stations = [exporting('alpha')];
    expect(foreignMachine('nas', stations)).toBeNull();
    expect(foreignMachine('/out', stations)).toBeNull();
    expect(foreignMachine('', stations)).toBeNull();
    expect(foreignMachine('nas/out', [{ info: null } as never])).toBeNull();
    expect(foreignMachine('nas/out', [])).toBeNull();
  });
});
