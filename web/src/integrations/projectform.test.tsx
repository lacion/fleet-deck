import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { IntegrationsStatus } from '../gen';
import { station } from '../testing/fleet';
import { emptyDraft } from './model';
import { hostAfterForge, ProjectForm } from './ProjectForm';

const status: IntegrationsStatus = {
  gh: { present: true, authenticated: true, user: 'ana', message: '' },
  glab: { present: false, authenticated: false, user: null, message: '' },
  trackers: [
    {
      id: 'jira-main',
      kind: 'jira',
      site: 'acme.atlassian.net',
      ok: true,
      user: 'ana',
      message: '',
    },
    { id: 'lin', kind: 'linear', site: null, ok: true, user: 'ana', message: '' },
  ],
};

const form = (
  draft = emptyDraft('station-a'),
  extra: Partial<Parameters<typeof ProjectForm>[0]> = {},
) =>
  renderToStaticMarkup(
    <ProjectForm
      stations={[station()]}
      draft={draft}
      onChange={() => {}}
      editing={false}
      saving={false}
      error={null}
      status={null}
      onSave={() => {}}
      onCancel={() => {}}
      {...extra}
    />,
  );

describe('the project form (DL-171)', () => {
  test('the host follows the forge while it is empty or the other default', () => {
    expect(hostAfterForge('github', '')).toBe('github.com');
    expect(hostAfterForge('gitlab', 'github.com')).toBe('gitlab.com');
    expect(hostAfterForge('github', 'gitlab.com')).toBe('github.com');
    expect(hostAfterForge('gitlab', 'git.corp.test')).toBe('git.corp.test');
    expect(hostAfterForge('none', 'github.com')).toBe('');
  });

  test('the host input holds the default as its value', () => {
    const html = form({ ...emptyDraft('station-a'), forge: 'github', host: 'github.com' });
    expect(html).toContain('value="github.com"');
    expect(html).toContain('placeholder="owner/name"');
    const lab = form({ ...emptyDraft('station-a'), forge: 'gitlab', host: 'gitlab.com' });
    expect(lab).toContain('value="gitlab.com"');
    expect(lab).toContain('placeholder="group/subgroup/name"');
  });

  test('the folder is a picker; the two prompts sit in Advanced', () => {
    const html = form();
    expect(html).toContain('class="fd-folder"');
    expect(html).toContain('<details class="fd-advanced"><summary>Advanced</summary>');
    expect(html).toContain('Prompt template');
    expect(html).toContain('Review prompt');
    expect(html).not.toContain('[[trackers]]');
    expect(html).toContain('>Save</button>');
    expect(html).toContain('>Cancel</button>');
  });

  test('a fresh form shows no error; a submit try shows them', () => {
    expect(form()).not.toContain('role="alert"');
    const tried = form(emptyDraft('station-a'), { initialTried: true });
    expect(tried).toContain('The name is 1 to 80 characters.');
    expect(tried).toContain('The path must be an absolute path on the station.');
  });

  test('with no account reported: the text and the link', () => {
    const html = form();
    expect(html).toContain('No Jira or Linear account on this station yet.');
    expect(html).toContain('<a href="#/settings/accounts">How to connect</a>');
  });

  test('the tracker rows: kind first, then the account select of that kind, then the key', () => {
    const draft = {
      ...emptyDraft('station-a'),
      trackers: [{ tracker: 'jira-main', kind: 'jira' as const, key: 'PROJ' }],
    };
    const html = form(draft, { status });
    expect(html).not.toContain('No Jira or Linear account');
    const kind = html.indexOf('>Kind<');
    const account = html.indexOf('>Account<');
    const key = html.indexOf('>Project key<');
    expect(kind).toBeGreaterThan(0);
    expect(account).toBeGreaterThan(kind);
    expect(key).toBeGreaterThan(account);
    expect(html).toContain(
      '<option value="jira-main" selected="">acme.atlassian.net (jira-main)</option>',
    );
    // A Linear account is not offered for a Jira row.
    expect(html).not.toContain('(lin)');
    expect(html).toContain('placeholder="PROJ"');
  });

  test('a Linear row offers the Linear accounts and the team key', () => {
    const draft = {
      ...emptyDraft('station-a'),
      trackers: [{ tracker: 'lin', kind: 'linear' as const, key: 'ENG' }],
    };
    const html = form(draft, { status });
    expect(html).toContain('<option value="lin" selected="">Linear (lin)</option>');
    expect(html).toContain('>Team key<');
    expect(html).toContain('placeholder="ENG"');
    expect(html).not.toContain('(jira-main)');
  });
});
