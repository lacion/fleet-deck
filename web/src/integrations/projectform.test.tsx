import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { AccountsList, AccountView } from '../gen';
import { station } from '../testing/fleet';
import { emptyDraft } from './model';
import {
  ContainerPicker,
  forgeUser,
  hostAfterForge,
  ProjectForm,
  trackerAccounts,
} from './ProjectForm';

const account = (
  extra: Partial<AccountView> & Pick<AccountView, 'account_id' | 'kind'>,
): AccountView => ({
  method: 'oauth',
  status: 'ok',
  message: '',
  ...extra,
});

const accounts: AccountsList = {
  accounts: [
    account({ account_id: 'jira-main', kind: 'jira', site: 'acme.atlassian.net', user: 'ana' }),
    account({ account_id: 'lin', kind: 'linear', method: 'token', user: 'ana' }),
    account({ account_id: 'gh-1', kind: 'github', method: 'cli', site: 'github.com', user: 'ana' }),
    account({ account_id: 'jira-old', kind: 'jira', method: 'cli' }),
  ],
  apps: [],
  env_tokens: [],
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
      accounts={null}
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
    expect(html).toContain('<a href="#/settings/accounts?station=station-a">How to connect</a>');
  });

  test('the tracker rows: kind first, then the account select of that kind, then the key', () => {
    const draft = {
      ...emptyDraft('station-a'),
      trackers: [{ tracker: 'jira-main', kind: 'jira' as const, key: 'PROJ' }],
    };
    const html = form(draft, { accounts });
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
    const html = form(draft, { accounts });
    expect(html).toContain('<option value="lin" selected="">Linear (lin)</option>');
    expect(html).toContain('>Team key<');
    expect(html).toContain('placeholder="ENG"');
    expect(html).not.toContain('(jira-main)');
  });

  test('the account select offers oauth, token and config accounts of the kind', () => {
    expect(trackerAccounts(accounts, 'jira').map((a) => a.account_id)).toEqual(['jira-main']);
    expect(trackerAccounts(accounts, 'linear').map((a) => a.account_id)).toEqual(['lin']);
    expect(trackerAccounts(null, 'jira')).toEqual([]);
  });

  test('the container select lists the projects of the account; the value is the key', () => {
    const html = renderToStaticMarkup(
      <ContainerPicker
        kind="jira"
        value="OPS"
        station="station-a"
        accountId="jira-main"
        load={undefined}
        onChange={() => {}}
        initial={{
          containers: [
            { key: 'PROJ', name: 'Platform' },
            { key: 'OPS', name: 'Operations' },
          ],
        }}
      />,
    );
    expect(html).toContain('>Jira project<');
    expect(html).toContain('<option value="OPS" selected="">Operations (OPS)</option>');
    expect(html).toContain('<option value="PROJ">Platform (PROJ)</option>');
    const linear = renderToStaticMarkup(
      <ContainerPicker
        kind="linear"
        value=""
        station="station-a"
        accountId="lin"
        load={undefined}
        onChange={() => {}}
        initial={{ containers: [{ key: 'ENG', name: 'Engineering' }] }}
      />,
    );
    expect(linear).toContain('>Linear team<');
  });

  test('without a list the key is typed', () => {
    const html = renderToStaticMarkup(
      <ContainerPicker
        kind="linear"
        value="ENG"
        station="station-a"
        accountId=""
        load={undefined}
        onChange={() => {}}
      />,
    );
    expect(html).toContain('>Team key<');
    expect(html).toContain('value="ENG"');
  });

  test('the forge shows who is signed in, or the way to connect', () => {
    expect(forgeUser(accounts, 'github', 'github.com')).toBe('ana');
    expect(forgeUser(accounts, 'github', '')).toBe('ana');
    expect(forgeUser(accounts, 'gitlab', 'gitlab.com')).toBeNull();
    const github = { ...emptyDraft('station-a'), forge: 'github' as const, host: 'github.com' };
    expect(form(github, { accounts })).toContain('Signed in as ana');
    const gitlab = { ...emptyDraft('station-a'), forge: 'gitlab' as const, host: 'gitlab.com' };
    const html = form(gitlab, { accounts });
    expect(html).toContain('Not connected. Connect it in');
    expect(html).toContain(
      '<a href="#/settings/accounts?station=station-a">Settings → Accounts</a>',
    );
    // The accounts did not load: the form says nothing about the login.
    expect(form(github)).not.toContain('Signed in as');
    expect(form(github)).not.toContain('Not connected');
  });
});
