import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { OtherBranch, OtherBranches } from '../integrations/actions';
import { IntegrationsProvider } from '../integrations/context';
import { fakeActions, project } from '../integrations/fixtures';
import {
  fetchedText,
  fetchFailure,
  foreignBranches,
  type GroupState,
  OtherStations,
  OtherStationsView,
  stationText,
} from './OtherStations';

const NOW = 1_760_000_000_000;
const row = (extra: Partial<OtherBranch> = {}): OtherBranch => ({
  name: 'fd/bravo/ana',
  slug: 'bravo',
  callsign: 'ana',
  head: 'a'.repeat(40),
  ahead: 2,
  behind: 1,
  at: NOW - 3_600_000,
  subject: 'Add the thing',
  legacy: false,
  station_id: 'st-b',
  station_name: 'Bravo',
  conflict: 'none',
  files: [],
  ...extra,
});
const data = (rows: OtherBranch[], extra: Partial<OtherBranches> = {}): OtherBranches => ({
  remote: 'origin',
  default_branch: 'main',
  truncated: false,
  branches: rows,
  ...extra,
});
const view = (state: GroupState, opts: { fetching?: boolean; fetchError?: string | null } = {}) =>
  renderToStaticMarkup(
    <OtherStationsView
      state={state}
      station="st-a"
      fetching={opts.fetching ?? false}
      fetchError={opts.fetchError ?? null}
      onFetch={() => {}}
      now={NOW}
    />,
  );
const ready = (rows: OtherBranch[], extra: Partial<OtherBranches> = {}): GroupState => ({
  kind: 'ready',
  data: data(rows, extra),
});

describe('the group Other stations (FG-10)', () => {
  test('a row has the branch, the station, the session link, the counts', () => {
    const html = view(
      ready([row({ session: { station_id: 'st-b', session_id: 's9', callsign: 'ana' } })]),
    );
    expect(html).toContain('Other stations');
    expect(html).toContain('<code class="fd-branch-name">fd/bravo/ana</code>');
    expect(html).toContain('Bravo');
    expect(html).toContain('href="#/s/st-b/s9"');
    expect(html).toContain('>ana</a>');
    expect(html).toContain('↑2');
    expect(html).toContain('↓1');
    expect(html).not.toContain('Will conflict');
  });

  test('no session says so; a legacy branch shows "legacy branch"', () => {
    const legacy: OtherBranch = {
      name: 'fd/ana',
      callsign: 'ana',
      head: 'b'.repeat(40),
      ahead: 1,
      behind: 0,
      at: NOW,
      subject: 's',
      legacy: true,
      conflict: 'none',
      files: [],
    };
    const html = view(ready([legacy]));
    expect(html).toContain('no session');
    expect(html).toContain('legacy branch');
  });

  test('the station column falls back to the id, then to "unknown station"', () => {
    const { station_name: _name, ...noName } = row();
    const { station_id: _id, ...noStation } = noName;
    expect(stationText(noName)).toBe('st-b');
    expect(stationText(noStation)).toBe('unknown station');
  });

  test('the branches of the own station are not listed', () => {
    const rows = [row({ station_id: 'st-a', name: 'own' }), row({ name: 'theirs' })];
    expect(foreignBranches(rows, 'st-a').map((r) => r.name)).toEqual(['theirs']);
    const html = view(ready(rows));
    expect(html).not.toContain('>own<');
    expect(html).toContain('theirs');
  });

  test('Will conflict counts the files, singular and plural', () => {
    const two = view(ready([row({ conflict: 'will_conflict', files: ['a', 'b'] })]));
    expect(two).toContain('Will conflict (2 files)');
    const one = view(ready([row({ conflict: 'will_conflict', files: ['a'] })]));
    expect(one).toContain('Will conflict (1 file)');
  });

  test('an empty list, and a list with only own branches, show the empty text', () => {
    const text = 'No branches of other stations. Push is off, or no other station pushed yet.';
    expect(view(ready([]))).toContain(text);
    expect(view(ready([row({ station_id: 'st-a' })]))).toContain(text);
    expect(view(ready([row()]))).not.toContain(text);
  });

  test('a cut list says so', () => {
    expect(view(ready([row()], { truncated: true }))).toContain(
      'Only the 200 newest branches are listed.',
    );
  });

  test('the Fetch button and the fetched line', () => {
    const html = view(ready([row()], { fetched_at: NOW - 5 * 60_000 }));
    expect(html).toContain('>Fetch</button>');
    expect(html).toContain('Fetched 5 min ago');
    expect(view(ready([row()], { fetched_at: NOW - 1000 }))).toContain('Fetched just now');
    expect(view(ready([row()]))).not.toContain('Fetched');
    const busy = view(ready([row()]), { fetching: true });
    expect(busy).toContain('Fetching…');
    expect(busy).toContain('disabled');
    expect(fetchedText(NOW - 3 * 3_600_000, NOW)).toBe('Fetched 3 h ago');
  });

  test('loading, a refusal and a failed fetch', () => {
    const loading = view({ kind: 'loading' });
    expect(loading).toContain('Loading the branches of other stations…');
    expect(loading).not.toContain('Fetch</button>');
    const refused = view({
      kind: 'error',
      message: 'This folder is not a verified home of the project. Check it on the Projects page.',
    });
    expect(refused).toContain('role="alert"');
    expect(refused).toContain('not a verified home');
    expect(refused).not.toContain('Try again');
    const failed = view(ready([row()]), { fetchError: 'The fetch failed.' });
    expect(failed).toContain('role="alert"');
    expect(failed).toContain('The fetch failed.');
  });

  test('a fetch answer fails with its text, else its code', () => {
    expect(fetchFailure({ fetched_at: 1 })).toBeNull();
    expect(fetchFailure({ code: 'fetch_failed', text: 'No route to host.' })).toBe(
      'No route to host.',
    );
    expect(fetchFailure({ code: 'fetch_failed' })).toBe('fetch_failed');
  });
});

describe('the container', () => {
  const fleet = { projectId: 'p1', station: 'st-a', session: 's1' };
  const synced = project({ git: { ...project().git, sync: true } });
  const mount = (projects: ReturnType<typeof project>[], canAdmin = true) =>
    renderToStaticMarkup(
      <IntegrationsProvider actions={fakeActions({ canAdmin })} initial={projects}>
        <OtherStations fleet={fleet} />
      </IntegrationsProvider>,
    );

  test('it shows for a synced project while the list loads', () => {
    expect(mount([synced])).toContain('Loading the branches of other stations…');
  });

  test('nothing for a project with sync off, an unknown project or no admin scope', () => {
    expect(mount([project()])).toBe('');
    expect(mount([])).toBe('');
    expect(mount([synced], false)).toBe('');
  });

  test('nothing without a provider', () => {
    expect(renderToStaticMarkup(<OtherStations fleet={fleet} />)).toBe('');
  });
});
