// FG-8 (design §11.8, §11.10, §11.11): the land panel, the status line and the project's Lands list.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DeckSession, Project } from '../gen';
import type { LandRow, ProjectLands as Lands } from '../integrations/actions';
import { IntegrationsProvider } from '../integrations/context';
import { fakeActions, project } from '../integrations/fixtures';
import { ProjectLands } from '../integrations/ProjectLands';
import { card, station } from '../testing/fleet';
import {
  LandPanel,
  landConfirmText,
  landLine,
  landOffered,
  phaseText,
  prNumber,
} from './LandPanel';

const synced = (extra: Partial<Project> = {}): Project =>
  project({ git: { ...project().git, sync: true, default_branch: 'main' }, ...extra });

const row = (extra: Partial<LandRow> = {}): LandRow => ({
  land_id: 'land-1',
  session_id: 's1',
  station_id: 'station-a',
  requested_by: 'owner',
  state: 'running',
  reconciling: false,
  attempt: 1,
  files: [],
  created_at: 1,
  ...extra,
});

const stations = [station('station-a')];

const session = (extra: Partial<DeckSession['summary']> = {}): DeckSession => ({
  ...card('s1', 'idle', 'station-a', {
    branch: 'fd/alpha/ada',
    worktree: '/work/atlas-wt',
    ...extra,
  }),
  project_id: 'p1',
});

const panel = (
  lands: Lands | null,
  s: DeckSession = session(),
  p: Project = synced(),
  canAdmin = true,
) =>
  renderToStaticMarkup(
    <IntegrationsProvider actions={fakeActions({ canAdmin })} initial={[p]}>
      <LandPanel session={s} stations={stations} initial={lands} />
    </IntegrationsProvider>,
  );

describe('the status line', () => {
  const line = (r: LandRow, p: Project | undefined = synced()) => landLine(r, p, stations);

  test('queued, running and the phase texts of the design', () => {
    expect(line(row({ state: 'queued', position: 2 })).text).toBe('Queued (2 ahead)');
    expect(line(row({ phase: 'captured' })).text).toBe('Landing: fetching');
    expect(line(row({ phase: 'worktree_ready' })).text).toBe('Landing: preparing a folder');
    expect(line(row({ phase: 'rebased' })).text).toBe('Landing: rebasing');
    expect(line(row({ phase: 'checking' })).text).toBe('Landing: running the check');
    expect(line(row({ phase: 'branch_push_intent' })).text).toBe('Landing: pushing the branch');
    expect(line(row({ phase: 'pr_intent' })).text).toBe('Landing: opening the pull request');
    expect(line(row({ phase: 'default_push_intent' })).text).toBe('Landing: pushing main');
    expect(phaseText('default_push_intent', undefined)).toBe('pushing the default branch');
  });

  test('the phases that the design does not list have their own words', () => {
    expect(phaseText('admitted', undefined)).toBe('starting');
    expect(phaseText('checked', undefined)).toBe('check passed');
    expect(phaseText('branch_pushed', undefined)).toBe('branch pushed');
    expect(phaseText('done', undefined)).toBe('finishing');
    expect(phaseText('unknown', undefined)).toBe('working');
    expect(phaseText(undefined, undefined)).toBe('working');
  });

  test('uncertain lands use the texts of §11.10', () => {
    expect(line(row({ reconciling: true })).text).toBe(
      'Checking the result of the land after an interruption…',
    );
    expect(line(row({ phase: 'lost' })).text).toBe(
      'The station has no record of this land. Check the remote, then press Stop checking.',
    );
    expect(line(row({ phase: 'lost', abandon_state: 'requested' })).text).toBe(
      'Stopping… waiting for Alpha to confirm.',
    );
  });

  test('landed shows a pull request link or the short oid', () => {
    const pr = line(row({ state: 'landed', pr_url: 'https://github.com/acme/atlas/pull/42' }));
    expect(pr.link).toEqual({
      href: 'https://github.com/acme/atlas/pull/42',
      label: 'Pull request #42',
    });
    expect(line(row({ state: 'landed', default_published_oid: 'abcdef0123456' })).text).toBe(
      'Landed abcdef0',
    );
    expect(line(row({ state: 'landed', pr_url: 'javascript:alert(1)' })).link).toBeUndefined();
    expect(prNumber('https://gitlab.com/a/b/-/merge_requests/7')).toBe('7');
    expect(prNumber('https://x.test/pr')).toBeNull();
  });

  test('a failure shows the text of the deck, with a fallback', () => {
    expect(
      line(row({ state: 'conflict', message: 'Conflict in 2 files with origin/main (abc1234).' }))
        .text,
    ).toBe('Conflict in 2 files with origin/main (abc1234).');
    expect(line(row({ state: 'conflict', files: ['a', 'b'], base_oid: 'abc1234def' })).text).toBe(
      'Conflict in 2 files (abc1234).',
    );
    expect(line(row({ state: 'failed', code: 'push_auth' })).text).toBe('push_auth');
    expect(line(row({ state: 'cancelled' })).text).toBe('Cancelled.');
  });
});

describe('the confirm text', () => {
  test('pull request mode', () => {
    expect(landConfirmText(synced())).toBe(
      'Fleet Deck rebases the branch onto origin/main in a separate folder, runs the check command, pushes the branch and opens or updates a pull request.',
    );
  });

  test('merge mode, and a missing default branch', () => {
    const p = project({
      forge: null,
      git: { ...project().git, sync: true, default_branch: null },
    });
    expect(landConfirmText(p)).toBe(
      'Fleet Deck rebases the branch onto the default branch of origin in a separate folder, runs the check command, pushes the branch, then pushes the result to the default branch of origin.',
    );
  });
});

describe('the Land button', () => {
  test('it shows for an idle session of a synced project', () => {
    const html = panel({ queue: [], recent: [] });
    expect(html).toContain('aria-label="Land"');
    expect(html).toContain('>Land</button>');
  });

  test('it hides without sync, without a branch, while the session works and during a land', () => {
    expect(landOffered(session(), project(), null)).toBe(false);
    expect(landOffered(session({ branch: null }), synced(), null)).toBe(false);
    expect(landOffered(session({ worktree: null }), synced(), null)).toBe(false);
    expect(landOffered(session({ current_turn: 't1' }), synced(), null)).toBe(false);
    expect(landOffered(session({ state: 'exited' }), synced(), null)).toBe(true);
    expect(landOffered(session(), synced(), row({ state: 'queued' }))).toBe(false);
    expect(landOffered(session(), synced(), row({ state: 'landed' }))).toBe(true);
  });

  test('nothing for an older deck, for a login without admin, or while the lands load', () => {
    expect(panel(null)).toBe('');
    expect(panel({ queue: [], recent: [] }, session(), synced(), false)).toBe('');
  });
});

describe('the status of the newest land of the session', () => {
  test('a queued land shows its position and Cancel land, and no Land button', () => {
    const html = panel({ queue: [row({ state: 'queued', position: 1 })], recent: [] });
    expect(html).toContain('Queued (1 ahead)');
    expect(html).toContain('Cancel land');
    expect(html).not.toContain('>Land</button>');
  });

  test('an uncertain land has Stop checking', () => {
    const html = panel({ queue: [row({ reconciling: true })], recent: [] });
    expect(html).toContain('Stop checking');
  });

  test('a land that is being stopped has no Stop checking; a removed station has Forget', () => {
    const stopping = row({ reconciling: true, abandon_state: 'requested' });
    expect(panel({ queue: [stopping], recent: [] })).not.toContain('>Stop checking<');
    expect(panel({ queue: [stopping], recent: [] })).not.toContain('>Forget<');
    const gone = row({ reconciling: true, abandon_state: 'requested', station_id: 'station-x' });
    const html = renderToStaticMarkup(
      <IntegrationsProvider actions={fakeActions()} initial={[synced()]}>
        <LandPanel
          session={{ ...session(), station_id: 'station-x' }}
          stations={stations}
          initial={{ queue: [gone], recent: [] }}
        />
      </IntegrationsProvider>,
    );
    expect(html).toContain('>Forget</button>');
  });

  test('a conflict lists its files and offers Send to session for an idle session', () => {
    const conflict = row({
      state: 'conflict',
      files: ['src/a.rs', 'src/b.rs'],
      message: 'Conflict in 2 files with origin/main (abc1234).',
    });
    const html = panel({ queue: [], recent: [conflict] });
    expect(html).toContain('Conflict in 2 files with origin/main (abc1234).');
    expect(html).toContain('<code>src/a.rs</code>');
    expect(html).toContain('>Send to session</button>');
    expect(html).toContain('>Land</button>');
  });

  test('an ended session cannot get the conflict: "Resume the session first"', () => {
    const conflict = row({ state: 'conflict', files: ['a'] });
    const html = panel({ queue: [], recent: [conflict] }, session({ state: 'exited' }));
    expect(html).toContain('Resume the session first');
    expect(html).not.toContain('>Send to session<');
  });

  test('a working session has the button disabled with the deck text', () => {
    const conflict = row({ state: 'conflict', files: ['a'] });
    const html = panel({ queue: [], recent: [conflict] }, session({ current_turn: 't1' }));
    expect(html).toContain('title="The session is working."');
  });

  test('the newest row of this session wins; other sessions are ignored', () => {
    const html = panel({
      queue: [row({ land_id: 'x', session_id: 'other', state: 'queued', position: 5 })],
      recent: [
        row({
          land_id: 'old',
          state: 'landed',
          default_published_oid: '1111111aaa',
          created_at: 1,
        }),
        row({
          land_id: 'new',
          state: 'landed',
          default_published_oid: '2222222bbb',
          created_at: 9,
        }),
      ],
    });
    expect(html).toContain('Landed 2222222');
    expect(html).not.toContain('1111111');
    expect(html).not.toContain('5 ahead');
  });
});

describe('the Lands list of the project page', () => {
  const list = (lands: Lands | null, p: Project = synced()) =>
    renderToStaticMarkup(
      <IntegrationsProvider actions={fakeActions()} initial={[p]}>
        <ProjectLands project={p} stations={stations} sessions={[session()]} initial={lands} />
      </IntegrationsProvider>,
    );

  test('it shows the queue and the recent ends, with the actions', () => {
    const html = list({
      queue: [row({ state: 'queued', position: 0, branch: 'fd/alpha/ada' })],
      recent: [
        row({ land_id: 'l2', state: 'failed', message: 'The check command failed (exit 1).' }),
      ],
    });
    expect(html).toContain('aria-label="Lands"');
    expect(html).toContain('Queue');
    expect(html).toContain('Recent');
    expect(html).toContain('<code>fd/alpha/ada</code>');
    expect(html).toContain('Queued (0 ahead)');
    expect(html).toContain('Cancel land');
    expect(html).toContain('The check command failed (exit 1).');
    expect(html).toContain('href="#/s/station-a/s1"');
  });

  test('empty lists say so for a synced project', () => {
    const html = list({ queue: [], recent: [] });
    expect(html).toContain('Nothing queued.');
    expect(html).toContain('No lands yet.');
  });

  test('it hides for an older deck, and for a project without sync and without lands', () => {
    expect(list(null)).toBe('');
    expect(list({ queue: [], recent: [] }, project())).toBe('');
    expect(list({ queue: [], recent: [row({ state: 'landed' })] }, project())).toContain('Lands');
  });
});
