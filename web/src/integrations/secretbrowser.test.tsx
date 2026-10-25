// Fleet Sync: the "Browse…" panel of the Secret files section and its call.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { SecretBrowseAnswer, SecretBrowseEntry } from '../gen';
import { ApiError } from '../runtime/api';
import { integrationActions } from './actions';
import { browsePath, reasonText, SecretBrowser } from './SecretBrowser';
import { appendPath } from './SecretFiles';

const homes = [
  { station_id: 'station-a', name: 'Alpha', path: '/srv/atlas' },
  { station_id: 'station-b', name: 'Beta', path: '/home/me/projects/atlas/' },
];

const entry = (extra: Partial<SecretBrowseEntry> & { name: string }): SecretBrowseEntry => ({
  kind: 'file',
  eligible: false,
  ...extra,
});

const answer = (extra: Partial<SecretBrowseAnswer> = {}): SecretBrowseAnswer => ({
  dir: '',
  entries: [],
  truncated: false,
  ...extra,
});

const panel = (
  props: Partial<Parameters<typeof SecretBrowser>[0]> & { answer?: SecretBrowseAnswer | null } = {},
) =>
  renderToStaticMarkup(
    <SecretBrowser
      homes={homes}
      station="station-a"
      answer={answer()}
      error={null}
      busy={false}
      draft=""
      onStation={() => {}}
      onDir={() => {}}
      onAdd={() => {}}
      onClose={() => {}}
      {...props}
    />,
  );

describe('browsePath', () => {
  test('the home folder has no leading slash', () => {
    expect(browsePath('', '.env')).toBe('.env');
    expect(browsePath('config', '.env.local')).toBe('config/.env.local');
    expect(browsePath('a/b', 'c')).toBe('a/b/c');
  });
});

describe('reasonText', () => {
  test('each reason has its text and an unknown code shows as it is', () => {
    expect(reasonText('not_ignored')).toBe('Git does not ignore it');
    expect(reasonText('not_member_hidden')).toBe('Members can read this name');
    expect(reasonText('too_large')).toBe('Larger than 512 KiB');
    expect(reasonText('symlink')).toBe('A link');
    expect(reasonText('private_file')).toBe('A private file of the station');
    expect(reasonText('not_regular')).toBe('Not a regular file');
    expect(reasonText('invalid_path')).toBe('Not a valid secret file path');
    expect(reasonText('unreadable')).toBe('Cannot be read');
    expect(reasonText('something_new')).toBe('something_new');
  });
});

describe('appendPath', () => {
  test('the path becomes a new last line', () => {
    expect(appendPath('', '.env')).toBe('.env');
    expect(appendPath('.env', 'a/.env')).toBe('.env\na/.env');
    expect(appendPath('.env\n', 'a/.env')).toBe('.env\na/.env');
  });
});

describe('the panel', () => {
  test('it is a region with the station select, the note and Close', () => {
    const html = panel();
    expect(html).toContain('role="region"');
    expect(html).toContain('aria-label="Browse the home"');
    expect(html).toContain('class="fd-secret-browse"');
    expect(html).toContain('Station');
    expect(html).toContain('<option value="station-a" selected="">Alpha</option>');
    expect(html).toContain('<option value="station-b">Beta</option>');
    expect(html).toContain(
      'Hidden files are shown. Only files that Git ignores and that members cannot read can be synced.',
    );
    expect(html).toContain('>Close</button>');
    expect(html).not.toContain('role="alert"');
    expect(html).not.toContain('role="status"');
  });

  test('the crumbs start with the name of the home folder and follow the folder', () => {
    const root = panel({ station: 'station-b' });
    expect(root).toContain('>atlas</button>');
    expect(root).toContain('aria-current="location"');
    const deep = panel({ answer: answer({ dir: 'config/sub' }) });
    expect(deep).toContain('>atlas</button>');
    expect(deep).toContain('>config</button>');
    expect(deep).toContain('>sub</button>');
    expect(deep.match(/aria-current="location"/g)).toHaveLength(1);
    expect(deep).toMatch(/aria-current="location"[^>]*>sub<\/button>/);
  });

  test('a folder is a button, an eligible file has Add and a size', () => {
    const html = panel({
      answer: answer({
        dir: 'config',
        entries: [
          entry({ name: '.hidden', kind: 'dir' }),
          entry({ name: '.env', eligible: true, size: 1229 }),
        ],
      }),
    });
    expect(html).toContain('aria-label="Open .hidden"');
    expect(html).toContain('1.2 KiB');
    expect(html).toMatch(/<button[^>]*class="fd-ghost"[^>]*>Add<\/button>/);
    expect(html).not.toContain('Added');
  });

  test('a file that is already in the draft shows a disabled Added', () => {
    const html = panel({
      draft: '.env\nconfig/.env.local\n',
      answer: answer({
        dir: 'config',
        entries: [entry({ name: '.env.local', eligible: true, size: 4 })],
      }),
    });
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>Added<\/button>/);
    expect(html).not.toContain('>Add</button>');
    // Another folder: the same name is another path.
    const other = panel({
      draft: '.env\nconfig/.env.local',
      answer: answer({ entries: [entry({ name: '.env.local', eligible: true, size: 4 })] }),
    });
    expect(other).toContain('>Add</button>');
  });

  test('a file that cannot be synced is muted with its reason and has no Add', () => {
    const html = panel({
      answer: answer({
        entries: [
          entry({ name: 'README.md', reason: 'not_member_hidden', size: 3 }),
          entry({ name: '.env.link', kind: 'other', reason: 'symlink' }),
          entry({ name: 'sock', kind: 'other', reason: 'not_regular' }),
          entry({ name: '.env.big', reason: 'too_large', size: 600_000 }),
          entry({ name: '.env.x', reason: 'not_ignored', size: 2 }),
          entry({ name: '.env.hard', reason: 'private_file', size: 2 }),
          entry({ name: 'weird', reason: 'something_new', size: 1 }),
        ],
      }),
    });
    expect(html).not.toContain('>Add</button>');
    expect(html.match(/data-muted="true"/g)).toHaveLength(7);
    for (const text of [
      'Members can read this name',
      'A link',
      'Not a regular file',
      'Larger than 512 KiB',
      'Git does not ignore it',
      'A private file of the station',
      'something_new',
      '585.9 KiB',
    ]) {
      expect(html).toContain(text);
    }
  });

  test('a truncated answer, an empty folder, loading and an error each have their text', () => {
    expect(panel({ answer: answer({ truncated: true }) })).toContain(
      'Only the first 500 entries are shown.',
    );
    expect(panel()).toContain('<p class="fd-note">This folder is empty.</p>');
    expect(panel({ answer: answer({ truncated: false }) })).not.toContain('first 500');
    const loading = panel({ answer: null, busy: true });
    expect(loading).toContain('<p role="status" class="fd-note">Loading…</p>');
    expect(loading).not.toContain('This folder is empty.');
    const failed = panel({ answer: null, error: 'The station did not answer.' });
    expect(failed).toContain('role="alert"');
    expect(failed).toContain('The station did not answer.');
  });
});

describe('browseSecretFiles', () => {
  const rig = (reply: unknown, fail?: ApiError) => {
    const calls: string[] = [];
    const io = {
      get: async (path: string) => {
        calls.push(path);
        if (fail !== undefined) throw fail;
        return reply;
      },
      post: async () => ({}),
      put: async () => ({}),
      del: async () => ({}),
    };
    return { calls, actions: integrationActions(io, ['read', 'drive', 'admin']) };
  };

  test('it reads the folder of a station with encoded query parameters', async () => {
    const reply = answer({ dir: 'a b', entries: [entry({ name: '.env' })] });
    const { calls, actions } = rig(reply);
    expect(await actions.browseSecretFiles('p 1', 'st/1', 'a b/c&d')).toEqual(reply);
    await actions.browseSecretFiles('p1', 'st-1', '');
    expect(calls).toEqual([
      '/api/projects/p%201/secret-files/browse?station=st%2F1&dir=a%20b%2Fc%26d',
      '/api/projects/p1/secret-files/browse?station=st-1&dir=',
    ]);
  });

  test('an answer that is not an answer is refused, and a refusal of the deck passes through', async () => {
    await expect(rig({ entries: [] }).actions.browseSecretFiles('p1', 's', '')).rejects.toThrow(
      'The deck sent an invalid secret browse.',
    );
    const refused = new ApiError(
      409,
      'This station has no ready home of the project.',
      'not_a_home',
    );
    await expect(rig({}, refused).actions.browseSecretFiles('p1', 's', '')).rejects.toBe(refused);
  });
});
