// FS-3 (design §3.6, §3.7): the Secret files section and its calls.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { ProjectHome, SecretFileReport, SecretFilesView, SecretStationReport } from '../gen';
import { ApiError } from '../runtime/api';
import { station } from '../testing/fleet';
import { integrationActions } from './actions';
import { IntegrationsProvider } from './context';
import { fakeActions, project } from './fixtures';
import { cellText, fileNote, parsePaths, SecretFiles, SecretFilesBody } from './SecretFiles';

const homeA = project().homes?.[0] as ProjectHome;
const homeB = (extra: Partial<ProjectHome> = {}): ProjectHome => ({
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

const stations = [station('station-a'), station('station-b')];
const two = project({ homes: [homeA, homeB()] });
const report = (extra: Partial<SecretFileReport> = {}): SecretFileReport => ({
  path: '.env',
  conflict: false,
  outcome: 'copied',
  stations: [],
  ...extra,
});
const cell = (
  state: SecretStationReport['state'],
  extra: Partial<SecretStationReport> = {},
): SecretStationReport => ({ station_id: 'station-b', state, ...extra });

const body = (
  view: Partial<SecretFilesView> = {},
  extra: Partial<Parameters<typeof SecretFilesBody>[0]> = {},
) =>
  renderToStaticMarkup(
    <SecretFilesBody
      project={two}
      stations={stations}
      view={{ paths: ['.env'], files: [], ...view }}
      draft=".env"
      busy={null}
      error={null}
      fieldError={null}
      onDraft={() => {}}
      onSave={() => {}}
      onSync={() => {}}
      {...extra}
    />,
  );

describe('parsePaths', () => {
  test('it trims each line and drops the empty lines', () => {
    expect(parsePaths('  .env \n\n   \nconfig/local.toml\r\n')).toEqual([
      '.env',
      'config/local.toml',
    ]);
    expect(parsePaths('')).toEqual([]);
  });
});

describe('cellText', () => {
  test('each state has its text', () => {
    expect(cellText(cell('same'))).toBe('Same');
    expect(cellText(cell('written'))).toBe('Copied');
    expect(cellText(cell('missing'))).toBe('Missing');
    expect(cellText(cell('offline'))).toBe('Offline');
    expect(cellText(cell('unknown'))).toBe('Unknown');
  });

  test('a refused file shows the text of its code', () => {
    const refused = (code?: string) =>
      cellText(cell('refused', code === undefined ? {} : { code }));
    expect(refused('not_ignored')).toBe('Not ignored by Git');
    expect(refused('symlink')).toBe('A link');
    expect(refused('too_large')).toBe('Larger than 512 KiB');
    expect(refused('not_regular')).toBe('Not a regular file');
    expect(refused('invalid_path')).toBe('Invalid path');
    expect(refused('private_file')).toBe('A private file of the station');
    expect(refused('other_code')).toBe('Refused');
    expect(refused()).toBe('Refused');
  });

  test('a failure shows its message, else its code, else unknown', () => {
    expect(cellText(cell('failed', { message: 'Disk full.', code: 'io' }))).toBe(
      'Failed: Disk full.',
    );
    expect(cellText(cell('failed', { code: 'io' }))).toBe('Failed: io');
    expect(cellText(cell('failed'))).toBe('Failed: unknown');
  });
});

describe('fileNote', () => {
  const at = 1_760_000_000_000;

  test('a synced file shows the time and the source station', () => {
    expect(fileNote(report({ synced_at: at, source: 'station-a' }), stations)).toBe(
      `Last sync ${new Date(at).toLocaleString()} from Alpha Copied to every home that was missing it or had an older copy.`,
    );
  });

  test('a run with nothing to copy and a run where all homes agree say so', () => {
    expect(fileNote(report({ outcome: 'nothing' }), stations)).toBe(
      'No home has this file yet, so there is nothing to copy. Create it in one home, then sync again.',
    );
    expect(fileNote(report({ outcome: 'same' }), stations)).toBe('Every home has the same copy.');
  });

  test('a file that never ran says so', () => {
    expect(fileNote(report({ outcome: 'not_run' }), stations)).toBe('Not synced yet.');
  });

  test('a conflict and a partial run add their sentences, joined by one space', () => {
    const note = fileNote(
      report({ synced_at: at, source: 'station-b', conflict: true, outcome: 'partial' }),
      stations,
    );
    expect(note).toBe(
      `Last sync ${new Date(at).toLocaleString()} from Beta Two homes changed this file. Beta had the newest copy; the other copies are in the station backups. Some homes did not get the file.`,
    );
  });
});

describe('the body of the section', () => {
  test('it shows the text area with the draft and the two buttons', () => {
    const html = body();
    expect(html).toContain('aria-label="Secret file paths"');
    expect(html).toContain('>.env</textarea>');
    expect(html).toContain('>Save</button>');
    expect(html).toContain('>Sync now</button>');
    expect(html).not.toContain('<table');
    expect(html).not.toContain('role="alert"');
  });

  test('the field says what to type; Save is a plain button, on only when the list changed', () => {
    const html = body();
    expect(html).toContain('placeholder=".env\nconfig/local.toml"');
    expect(html).toContain(
      '<span id="fd-int-secret-hint" class="fd-fieldhint">One path per line, relative to the project folder.</span>',
    );
    expect(html).toContain('<button type="button" disabled="">Save</button>');
    expect(body({}, { draft: '.env\na.key' })).toContain('<button type="button">Save</button>');
    expect(html).toMatch(/<button type="button" class="fd-ghost"[^>]*>Sync now<\/button>/);
  });

  test('Sync now is off with no saved path, with fewer than two ready homes and while busy', () => {
    const off = /<button[^>]*disabled=""[^>]*>Sync now<\/button>/;
    expect(body()).not.toMatch(off);
    expect(body({ paths: [] })).toMatch(off);
    expect(body({}, { project: project({ homes: [homeA, homeB({ state: 'failed' })] }) })).toMatch(
      off,
    );
    expect(body({}, { project: project() })).toMatch(off);
    expect(body({}, { busy: 'save' })).toMatch(off);
  });

  test('Browse… shows next to Save when the project has a ready home and the page can browse', () => {
    const button = '>Browse…</button>';
    expect(body({}, { onBrowse: () => {} })).toContain(button);
    expect(body()).not.toContain(button);
    expect(
      body({}, { onBrowse: () => {}, project: project({ homes: [homeA, homeB()] }) }),
    ).toContain(button);
    expect(
      body(
        {},
        {
          onBrowse: () => {},
          project: project({ homes: [{ ...homeA, state: 'failed' }] }),
        },
      ),
    ).not.toContain(button);
  });

  test('the browser panel shows below the buttons', () => {
    const html = body({}, { browser: <div className="fd-secret-browse">panel</div> });
    expect(html).toContain('<div class="fd-secret-browse">panel</div>');
  });

  test('while a sync runs the button says Syncing…', () => {
    expect(body({}, { busy: 'sync' })).toMatch(/<button[^>]*disabled=""[^>]*>Syncing…<\/button>/);
  });

  test('a paths error and a general error show as alerts', () => {
    const html = body(
      {},
      {
        fieldError: 'A secret file is a relative path.',
        error: 'Set the forge of the project first.',
      },
    );
    expect(html.match(/role="alert"/g)).toHaveLength(2);
    expect(html).toContain('A secret file is a relative path.');
    expect(html).toContain('Set the forge of the project first.');
  });

  test('the table has a column for each home, a cell text for each station and the notes', () => {
    const files = [
      report({
        path: '.env',
        synced_at: 1_760_000_000_000,
        source: 'station-a',
        stations: [cell('written')],
      }),
      report({ path: 'a.key', outcome: 'not_run' }),
      report({
        path: 'b.key',
        conflict: true,
        outcome: 'partial',
        source: 'station-a',
        stations: [cell('refused', { code: 'not_ignored' })],
      }),
    ];
    const html = body({ paths: ['.env', 'a.key', 'b.key'], files });
    expect(html).toContain('class="fd-int-secret-table"');
    expect(html).toContain('<th scope="col">File</th>');
    expect(html).toContain('<th scope="col">Alpha</th>');
    expect(html).toContain('<th scope="col">Beta</th>');
    expect(html).toContain('<code>.env</code>');
    expect(html).toContain(
      '<td data-state="written"><span class="fd-int-cell" data-tone="ok"><span class="fd-dot" data-status="online" aria-hidden="true"></span>Copied</span></td>',
    );
    expect(html).toMatch(
      /<td data-state="refused"><span class="fd-int-cell" data-tone="hazard"><svg[^>]*lucide-triangle-alert[\s\S]*?<\/svg>Not ignored by Git<\/span><\/td>/,
    );
    expect(html).toContain('<td>—</td>');
    expect(html).toContain('Last sync');
    expect(html).toContain('Not synced yet.');
    expect(html).toContain(
      'Two homes changed this file. Alpha had the newest copy; the other copies are in the station backups.',
    );
    expect(html).toContain('Some homes did not get the file.');
    expect(html).toContain('colSpan="3"');
  });
});

describe('the section', () => {
  const section = (canAdmin: boolean) =>
    renderToStaticMarkup(
      <IntegrationsProvider actions={fakeActions({ canAdmin })} initial={[two]}>
        <SecretFiles project={two} stations={stations} />
      </IntegrationsProvider>,
    );

  test('before the first answer it shows the heading, the text and Loading…, with no text area', () => {
    const html = section(true);
    expect(html).toContain('aria-label="Secret files"');
    expect(html).toContain('Secret files</h3>');
    expect(html).toContain(
      'Files that Git ignores, such as .env, copied between the homes of this project. The newest copy wins; the replaced copy is kept on its station.',
    );
    expect(html).toContain('Loading…');
    expect(html).not.toContain('<textarea');
  });

  test('it shows nothing without the admin login', () => {
    expect(section(false)).toBe('');
    expect(renderToStaticMarkup(<SecretFiles project={two} stations={stations} />)).toBe('');
  });
});

describe('the calls', () => {
  const view: SecretFilesView = { paths: ['.env'], files: [] };
  const rig = (reply: unknown = view, fail?: ApiError) => {
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

  test('the three routes, methods and bodies', async () => {
    const { calls, actions } = rig();
    expect(await actions.secretFiles('p 1')).toEqual(view);
    expect(await actions.setSecretFiles('p1', ['.env', 'a/b'])).toEqual(view);
    expect(calls.map(([m, p]) => `${m} ${p}`)).toEqual([
      'GET /api/projects/p%201/secret-files',
      'PUT /api/projects/p1/secret-files',
    ]);
    expect(calls[1]?.[2]).toMatchObject({
      paths: ['.env', 'a/b'],
      command_id: expect.stringMatching(/^[0-9a-f]{32}$/),
    });
    const sync = rig({ files: [] });
    expect(await sync.actions.syncSecretFiles('p1')).toEqual({ files: [] });
    expect(sync.calls[0]?.[0]).toBe('POST');
    expect(sync.calls[0]?.[1]).toBe('/api/projects/p1/secret-files/sync');
    expect(sync.calls[0]?.[2]).toMatchObject({
      command_id: expect.stringMatching(/^[0-9a-f]{32}$/),
    });
  });

  test('an invalid answer is refused', async () => {
    await expect(rig({}).actions.secretFiles('p1')).rejects.toThrow('invalid secret files');
    await expect(rig({}).actions.setSecretFiles('p1', [])).rejects.toThrow('invalid secret files');
    await expect(rig({}).actions.syncSecretFiles('p1')).rejects.toThrow('invalid secret sync');
  });

  test('an ApiError passes through with its code and field', async () => {
    const refused = rig(
      {},
      new ApiError(400, 'A secret file is a relative path.', 'invalid_secret_path', 'paths'),
    );
    await expect(refused.actions.setSecretFiles('p1', ['../x'])).rejects.toMatchObject({
      status: 400,
      code: 'invalid_secret_path',
      field: 'paths',
    });
    const needs = rig({}, new ApiError(409, 'Set the forge of the project first.', 'needs_remote'));
    await expect(needs.actions.syncSecretFiles('p1')).rejects.toMatchObject({
      code: 'needs_remote',
    });
  });
});
