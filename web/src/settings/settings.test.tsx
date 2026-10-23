import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { RestartAnswer, SettingsSaved } from '../gen';
import { More } from '../app/More';
import { Sidebar } from '../app/Sidebar';
import type { SettingsActions } from './actions';
import { SettingsProvider } from './context';
import { ElevationDialog, RestartDialog, RestartOverlay, SecretDialog, waitStep } from './dialogs';
import { elevate, keysOfError, relaySecret, tryRestart, trySave, UNSUPPORTED_TEXT } from './flow';
import { FIELDS, meta, VALUES, view } from './fixtures';
import { DeckError } from './http';
import { CARD_KEYS, changedKeys, changesOf, groupFields } from './model';
import { Notice, noticesOfSaved, SaveBar, SettingsTab } from './SettingsTab';
import { SettingsFields } from './SettingsFields';

const noop = () => {};

const fields = (extra: Partial<Parameters<typeof SettingsFields>[0]> = {}) =>
  renderToStaticMarkup(
    <SettingsFields
      view={view()}
      edits={{}}
      errors={{}}
      onEdit={noop}
      stationId="st-alpha"
      savedIds={['claude']}
      secretBlocked={null}
      onSecret={noop}
      {...extra}
    />,
  );

const saved = (extra: Partial<SettingsSaved> = {}): SettingsSaved => ({
  values: VALUES,
  applied: [],
  restart_needed: [],
  unapplied: [],
  revision: 'rev-2',
  durability: 'confirmed',
  ...extra,
});

/** Fake actions: `calls` records what the board sent; each call answers with `reply`. */
function fake(reply: {
  save?: () => Promise<SettingsSaved>;
  setSecret?: () => Promise<SettingsSaved>;
  elevate?: () => Promise<void>;
  restart?: () => Promise<RestartAnswer>;
  elevation?: string | null;
}) {
  const calls: { name: string; arg: unknown }[] = [];
  let held: string | null = reply.elevation ?? null;
  const actions: SettingsActions = {
    view: async () => view(),
    save: async (_t, change) => {
      calls.push({ name: 'save', arg: change });
      return (reply.save ?? (async () => saved()))();
    },
    setSecret: async (_s, input) => {
      calls.push({ name: 'setSecret', arg: input });
      return (reply.setSecret ?? (async () => saved()))();
    },
    elevate: async (c) => {
      calls.push({ name: 'elevate', arg: c });
      await (reply.elevate ?? (async () => {}))();
      held = 'el-1';
    },
    elevation: () => held,
    forgetElevation: () => {
      held = null;
    },
    restart: async (_t, stop, elevation) => {
      calls.push({ name: 'restart', arg: { stop, elevation } });
      return (reply.restart ?? (async () => ({ restarting: true, would_stop: [] })))();
    },
  };
  return { actions, calls };
}

const fail = (status: number, code: string, message: string, details = {}) =>
  Promise.reject(new DeckError(status, message, code, details));

describe('the sections of the page (DL-172)', () => {
  test('the groups keep the order of the brief; an unknown key goes to Other', () => {
    const titles = groupFields(FIELDS).map((g) => g.title);
    expect(titles).toEqual([
      'General',
      'Sessions',
      'Folders',
      'Shells and setup',
      'Providers',
      'Model help',
      'Sharing',
      'Network',
      'Trackers',
      'Other',
    ]);
    const sessions = groupFields(FIELDS).find((g) => g.title === 'Sessions');
    expect(sessions?.fields.map((f) => f.key)).toEqual(['max_sessions', 'host']);
    const other = groupFields(FIELDS).find((g) => g.title === 'Other');
    expect(other?.fields.map((f) => f.key)).toEqual(['zz_future']);
    const deck = groupFields([
      meta('conflict_mail', 'Conflict mail', 'bool'),
      meta('link_listen', 'Links', 'addresses'),
      meta('tailnet_hostname', 'Tailnet', 'text'),
    ]);
    expect(deck.map((g) => g.title)).toEqual(['Network', 'Notifications']);
    expect(deck[0]?.fields.map((f) => f.key)).toEqual(['link_listen', 'tailnet_hostname']);
  });

  test('a hidden field and a key of the Tailnet card are left out, also from Other', () => {
    // While the Tailnet card has its view it shows and saves tailnet_auto and tailscale_cert
    // (`omit`); when it cannot read Tailscale, the fields stay, so the keys can still change.
    const all = [
      meta('listen', 'Listen', 'addresses'),
      meta('tailnet_auto', 'Use Tailscale automatically', 'bool'),
      meta('tailscale_cert', 'Trusted certificate from Tailscale', 'bool'),
      meta('tailnet_auto_listen', 'Added automatically', 'addresses', { hidden: true }),
      meta('zz_hidden', 'Hidden future key', 'text', { hidden: true }),
      meta('zz_shown', 'Shown future key', 'text', { hidden: false }),
    ];
    const groups = groupFields(all, [], CARD_KEYS);
    expect(groups.map((g) => g.title)).toEqual(['Network', 'Other']);
    expect(groups[0]?.fields.map((f) => f.key)).toEqual(['listen']);
    expect(groups[1]?.fields.map((f) => f.key)).toEqual(['zz_shown']);
    const failed = groupFields(all).find((g) => g.title === 'Network');
    expect(failed?.fields.map((f) => f.key)).toEqual(['listen', 'tailnet_auto', 'tailscale_cert']);
  });

  test('a kept section shows without a field, in its place', () => {
    const groups = groupFields(
      [meta('name', 'Name', 'text'), meta('conflict_mail', 'Mail', 'bool')],
      ['Network'],
    );
    expect(groups.map((g) => g.title)).toEqual(['General', 'Network', 'Notifications']);
    expect(groups[1]?.fields).toEqual([]);
  });

  test('the desktop keys have their section after Shells and setup', () => {
    const groups = groupFields([
      meta('fleet_summary', 'Fleet summary', 'bool'),
      meta('allow_desktop', 'Desktop control', 'bool'),
      meta('allow_shell', 'Allow terminals', 'bool'),
      meta('max_sessions', 'Most sessions', 'int'),
      meta('capture_monitors', 'Screenshot monitors', 'texts'),
    ]);
    expect(groups.map((g) => g.title)).toEqual(['Sessions', 'Shells and setup', 'Desktop']);
    expect(groups[2]?.fields.map((f) => f.key)).toEqual([
      'allow_desktop',
      'capture_monitors',
      'fleet_summary',
    ]);
  });

  test('the fleet control keys have their section and tailscale_bin is in Network (ADR-008)', () => {
    const groups = groupFields([
      meta('allow_fleet_control', 'Allow fleet control', 'bool'),
      meta('fleet_control', 'Fleet control', 'bool'),
      meta('tailscale_bin', 'Tailscale binary', 'text'),
      meta('listen', 'Listen', 'text'),
    ]);
    expect(groups.map((g) => g.title)).toEqual(['Fleet control', 'Network']);
    expect(groups[0]?.fields.map((f) => f.key)).toEqual(['fleet_control', 'allow_fleet_control']);
    expect(groups[1]?.fields.map((f) => f.key)).toEqual(['listen', 'tailscale_bin']);
  });
});

describe('field rendering for each kind', () => {
  const html = fields();

  test('a bool is a switch', () => {
    expect(html).toContain('role="switch"');
    expect(html).toContain('type="checkbox"');
  });

  test('an int is a number input with min and max', () => {
    expect(html).toContain('type="number"');
    expect(html).toContain('min="1"');
    expect(html).toContain('max="256"');
    expect(html).toContain('value="8"');
  });

  test('a text is an input; an enum is a select of its options', () => {
    expect(html).toContain('value="Alpha"');
    // The host options show words; the values stay the same.
    expect(html).toContain('<option value="direct">Directly</option>');
    expect(html).toContain(
      '<option value="shim" selected="">Apart from the station (survives restarts)</option>',
    );
    // The provider of model help shows its name, and the empty option says what it means.
    expect(html).toContain('<option value="claude">Claude</option>');
    expect(html).toContain(
      '<option value="" selected="">Default (the first signed-in provider)</option>',
    );
  });

  test('paths are folder pickers with Remove and "Add folder" on a station tab', () => {
    expect(html).toContain('class="fd-folder"');
    expect(html).toContain('aria-label="Remove Allowed folders 1"');
    expect(html).toContain('Add folder');
    // A settings row has no pin button: pinning belongs to the New session sheet.
    expect(html).not.toContain('Pin this directory');
    // The deck tab has no folders to browse: plain inputs.
    const deck = fields({ stationId: null });
    expect(deck).not.toContain('class="fd-folder"');
    expect(deck).toContain('value="/work"');
  });

  test('texts, addresses and repository groups are lists of rows', () => {
    expect(html).toContain('aria-label="Hidden file patterns 1"');
    expect(html).toContain('value="*.pem"');
    expect(html).toContain('placeholder="127.0.0.1:7421"');
    expect(html).toContain('aria-label="Repository path 1"');
    expect(html).toContain('aria-label="Group name 1"');
    expect(html).toContain('value="/work/app"');
  });

  test('trackers are a read-only table with the note', () => {
    expect(html).toContain('aria-label="Trackers from the config file"');
    expect(html).toContain('acme.atlassian.net');
    expect(html).toContain('env JIRA_TOKEN');
    expect(html).toContain(
      'From the config file of this station. Connect accounts in the Accounts tab.',
    );
  });

  test('the instances table has a row with the env variables as "set"', () => {
    expect(html).toContain('aria-label="Provider logins"');
    expect(html).toContain('<code>claude</code>');
    expect(html).toContain('ANTHROPIC_API_KEY');
    expect(html).toContain('>set</span>');
    expect(html).toContain('aria-label="Replace ANTHROPIC_API_KEY"');
    expect(html).toContain('aria-label="Remove ANTHROPIC_API_KEY"');
    expect(html).toContain('Add variable');
    // A value is never in the page.
    expect(html).not.toContain('sk-');
  });

  test('the secret buttons are off when the logins have unsaved edits', () => {
    const blocked = fields({ secretBlocked: 'Save or discard your changes to the logins.' });
    expect(blocked).toContain('Save or discard your changes to the logins.');
    expect(blocked).toMatch(/<button[^>]*disabled=""[^>]*aria-label="Replace ANTHROPIC_API_KEY"/);
  });

  test('an error shows on its field', () => {
    expect(fields({ errors: { allowed_roots: '`allowed_roots` has an invalid value' } })).toContain(
      '`allowed_roots` has an invalid value',
    );
  });
});

describe('the changes of an edit', () => {
  const v = view();

  test('only the changed keys are sent', () => {
    expect(changesOf(v, {})).toEqual({});
    expect(changesOf(v, { name: 'Alpha', max_sessions: 8 })).toEqual({});
    expect(changesOf(v, { name: 'Beta', max_sessions: 8 })).toEqual({ name: 'Beta' });
    expect(changedKeys(v, { max_sessions: 9, name: 'Beta' })).toEqual(['name', 'max_sessions']);
  });

  test('a cleared list or optional value is null', () => {
    expect(changesOf(v, { member_deny: [] })).toEqual({ member_deny: null });
    expect(changesOf(v, { member_deny: [''] })).toEqual({ member_deny: null });
    expect(changesOf(v, { max_sessions: null })).toEqual({ max_sessions: null });
    expect(changesOf(v, { name: '  ' })).toEqual({ name: null });
    expect(changesOf(v, { repo_groups: [] })).toEqual({ repo_groups: null });
    // A value that was already unset and stays empty is no change.
    expect(changesOf(v, { 'model_help.provider': '' })).toEqual({});
  });

  test('a list keeps its non-empty rows; a blank row is not a change', () => {
    expect(changesOf(v, { allowed_roots: ['/work', '/srv', ''] })).toEqual({});
    expect(changesOf(v, { allowed_roots: ['/work', '/srv', '/new'] })).toEqual({
      allowed_roots: ['/work', '/srv', '/new'],
    });
    expect(
      changesOf(v, {
        repo_groups: [
          ['/work/app', 'app'],
          ['', ''],
        ],
      }),
    ).toEqual({});
    expect(changesOf(v, { repo_groups: [['/work/app', 'core']] })).toEqual({
      repo_groups: { '/work/app': 'core' },
    });
  });

  test('a command line key, a read-only key and an unknown key are never sent', () => {
    expect(changesOf(v, { listen: ['0.0.0.0:1'], trackers: [], nope: 1 })).toEqual({});
  });

  test('the logins keep {set:true} for a kept variable and null for a removed one', () => {
    const logins = [
      {
        id: 'claude',
        driver: 'claude',
        binary: 'claude',
        config_home: null,
        env: { ANTHROPIC_API_KEY: { set: true }, OTHER: null },
      },
    ];
    expect(changesOf(v, { provider_instance: logins })).toEqual({ provider_instance: logins });
    // No instance is an empty list, not null (a missing key would mean the defaults).
    expect(changesOf(v, { provider_instance: [] })).toEqual({ provider_instance: [] });
  });
});

describe('the save outcomes', () => {
  const known = FIELDS.map((f) => f.key);

  test('a success sends the changes and the revision', async () => {
    const { actions, calls } = fake({
      save: async () => saved({ restart_needed: ['host', 'listen'], applied: ['name'] }),
    });
    const out = await trySave(actions, { kind: 'deck' }, { name: 'B' }, 'rev-1', known);
    expect(out.kind).toBe('saved');
    expect(calls[0]).toEqual({
      name: 'save',
      arg: { changes: { name: 'B' }, revision: 'rev-1', elevation: null },
    });
  });

  test('restart_needed gives a notice with its count; uncertain gives a warning', () => {
    const n = noticesOfSaved(saved({ restart_needed: ['host', 'listen'] }));
    expect(n).toEqual([{ id: 'restart', tone: 'info', text: '2 settings apply after a restart.' }]);
    expect(noticesOfSaved(saved({ restart_needed: ['host'] }))[0]?.text).toBe(
      '1 setting applies after a restart.',
    );
    const u = noticesOfSaved(saved({ durability: 'uncertain' }));
    expect(u).toEqual([
      {
        id: 'uncertain',
        tone: 'warn',
        text: 'Saved and applied. The disk did not confirm the write; check the disk.',
      },
    ]);
    expect(noticesOfSaved(saved())).toEqual([]);
  });

  test('elevation_required asks for the dialog; the retry carries the elevation', async () => {
    let first = true;
    const { actions, calls } = fake({
      save: () => {
        if (first) {
          first = false;
          return fail(400, 'elevation_required', 'a recent sign-in is needed', {
            keys: ['allow_shell'],
          });
        }
        return Promise.resolve(saved());
      },
    });
    const out = await trySave(actions, { kind: 'deck' }, { allow_shell: true }, 'rev-1', known);
    expect(out).toEqual({ kind: 'elevate', keys: ['allow_shell'] });
    expect(await elevate(actions, { password: 'pw' })).toEqual({ kind: 'ok' });
    const retry = await trySave(actions, { kind: 'deck' }, { allow_shell: true }, 'rev-1', known);
    expect(retry.kind).toBe('saved');
    expect(
      calls
        .filter((c) => c.name === 'save')
        .map((c) => (c.arg as { elevation: unknown }).elevation),
    ).toEqual([null, 'el-1']);
  });

  test('a rejected elevation is forgotten', async () => {
    const { actions } = fake({
      elevation: 'old',
      save: () => fail(400, 'elevation_required', 'again', { keys: [] }),
    });
    const out = await trySave(actions, { kind: 'deck' }, {}, 'r', known);
    expect(out.kind).toBe('elevate');
    expect(actions.elevation()).toBeNull();
  });

  test('settings_changed and settings_changed_on_disk give "changed"', async () => {
    for (const code of ['settings_changed', 'settings_changed_on_disk']) {
      const { actions } = fake({ save: () => fail(409, code, 'x') });
      expect((await trySave(actions, { kind: 'deck' }, {}, 'r', known)).kind).toBe('changed');
    }
  });

  test('invalid and cannot_apply name the fields', async () => {
    const a = fake({
      save: () => fail(400, 'invalid', '`allowed_roots` has an invalid value'),
    });
    expect(await trySave(a.actions, { kind: 'deck' }, {}, 'r', known)).toEqual({
      kind: 'invalid',
      keys: ['allowed_roots'],
      text: '`allowed_roots` has an invalid value',
    });
    const b = fake({
      save: () => fail(400, 'cannot_apply', '`provider_instance.x.binary` cannot be applied: no'),
    });
    expect(await trySave(b.actions, { kind: 'deck' }, {}, 'r', known)).toMatchObject({
      kind: 'invalid',
      keys: ['provider_instance'],
    });
    const c = fake({ save: () => fail(400, 'invalid', 'bad', { keys: ['max_sessions', 'name'] }) });
    expect(await trySave(c.actions, { kind: 'deck' }, {}, 'r', known)).toMatchObject({
      keys: ['max_sessions', 'name'],
    });
    const e = new DeckError(400, '`zzz` has an invalid value', 'invalid', {});
    expect(keysOfError(e, known)).toEqual([]);
  });

  test('any other error is a message', async () => {
    const { actions } = fake({
      save: () => fail(500, 'settings_write_failed', 'the settings file could not be written'),
    });
    expect(await trySave(actions, { kind: 'deck' }, {}, 'r', known)).toEqual({
      kind: 'failed',
      text: 'the settings file could not be written',
    });
    const net = fake({ save: () => Promise.reject(new Error('Failed to fetch')) });
    expect(await trySave(net.actions, { kind: 'deck' }, {}, 'r', known)).toEqual({
      kind: 'failed',
      text: 'Failed to fetch',
    });
  });

  test('the tab shows its notices, the Reload button and the save bar', () => {
    const html = renderToStaticMarkup(
      <SettingsTab
        stationId="st-alpha"
        name="Alpha"
        actions={fake({}).actions}
        live
        initial={{
          view: view(),
          edits: { name: 'Beta' },
          notices: [
            { id: 'restart', tone: 'info', text: '1 setting applies after a restart.' },
            { id: 'changed', tone: 'warn', text: 'These settings changed since you opened them.' },
            {
              id: 'uncertain',
              tone: 'warn',
              text: 'Saved and applied. The disk did not confirm the write; check the disk.',
            },
          ],
        }}
      />,
    );
    expect(html).toContain('Restart Alpha…');
    expect(html).toContain('These settings changed since you opened them.');
    expect(html).toContain('>Reload</button>');
    expect(html).toContain('The disk did not confirm the write; check the disk.');
    expect(html).toContain('Unsaved changes <span class="fd-count">1</span>');
    expect(html).toContain('>Discard</button>');
    expect(html).toContain('>Save</button>');
    // No edit: no bar.
    const clean = renderToStaticMarkup(
      <SettingsTab
        stationId={null}
        name="deck"
        actions={fake({}).actions}
        live
        initial={{ view: view(), edits: { name: 'Alpha' } }}
      />,
    );
    expect(clean).not.toContain('fd-set-bar');
    expect(clean).not.toContain('Unsaved changes');
  });

  test('the bar and a notice render alone', () => {
    const bar = renderToStaticMarkup(
      <SaveBar count={3} saving={false} onDiscard={noop} onSave={noop} />,
    );
    expect(bar).toContain('Unsaved changes <span class="fd-count">3</span>');
    expect(bar).toContain('fd-ghost');
    expect(bar).toContain('fd-primary');
    expect(
      renderToStaticMarkup(<Notice item={{ id: 'error', tone: 'error', text: 'Boom' }} />),
    ).toContain('role="alert"');
  });
});

describe('the elevation dialog (DL-172 1.5)', () => {
  const dialog = (extra: Partial<Parameters<typeof ElevationDialog>[0]> = {}) =>
    renderToStaticMarkup(
      <ElevationDialog
        labels={[]}
        busy={false}
        error={null}
        onSubmit={noop}
        onCancel={noop}
        {...extra}
      />,
    );

  test('the title, the password, the code and the hint', () => {
    const html = dialog({ labels: ['Allow terminals'] });
    expect(html).toContain('Confirm it is you');
    expect(html).toContain('type="password"');
    expect(html).toContain('No password? Run <code>fleetdeck open --elevate</code>');
    expect(html).toContain('Code');
    expect(html).toContain('Allow terminals');
    expect(html).toContain('aria-modal="true"');
  });

  test('an error and a busy state', () => {
    expect(dialog({ error: 'Wrong password or code.' })).toContain('Wrong password or code.');
    expect(dialog({ busy: true })).toContain('Checking…');
  });

  test('401 is a wrong password; 429 is too many tries; the elevation stays in memory', async () => {
    const wrong = fake({ elevate: () => fail(401, 'invalid_credentials', 'x') });
    expect(await elevate(wrong.actions, { password: 'nope' })).toEqual({
      kind: 'failed',
      text: 'Wrong password or code.',
    });
    expect(wrong.actions.elevation()).toBeNull();
    const many = fake({ elevate: () => fail(429, 'too_many_attempts', 'x') });
    expect(await elevate(many.actions, { code: 'ABC' })).toEqual({
      kind: 'failed',
      text: 'Too many tries. Wait a minute.',
    });
    const ok = fake({});
    expect(await elevate(ok.actions, { code: 'ABCDE12345' })).toEqual({ kind: 'ok' });
    expect(ok.calls[0]).toEqual({ name: 'elevate', arg: { code: 'ABCDE12345' } });
    expect(ok.actions.elevation()).toBe('el-1');
  });
});

describe('the secret dialog (DL-172 6)', () => {
  test('a password input with the sentence; no value is rendered', () => {
    const html = renderToStaticMarkup(
      <SecretDialog
        title="Replace ANTHROPIC_API_KEY"
        name="ANTHROPIC_API_KEY"
        busy={false}
        error={null}
        onSubmit={noop}
        onCancel={noop}
      />,
    );
    expect(html).toContain('type="password"');
    expect(html).toContain('Fleet Deck sends this once to the station and never shows it again.');
    expect(html).not.toContain('value=');
    const add = renderToStaticMarkup(
      <SecretDialog
        title="Add a variable"
        name={null}
        busy={false}
        error={null}
        onSubmit={noop}
        onCancel={noop}
      />,
    );
    expect(add).toContain('Variable name');
  });

  test('the relay clears the input before the call, and sends the key with the revision', async () => {
    let typed = 'sk-live-123';
    const order: string[] = [];
    const { actions, calls } = fake({
      elevation: 'el-9',
      setSecret: async () => {
        order.push('call');
        return saved();
      },
    });
    const out = await relaySecret(actions, {
      station: 'st-alpha',
      key: 'provider_instance.claude.env.ANTHROPIC_API_KEY',
      revision: 'rev-1',
      read: () => typed,
      clear: () => {
        order.push('clear');
        typed = '';
      },
    });
    expect(out.kind).toBe('saved');
    expect(order).toEqual(['clear', 'call']);
    expect(typed).toBe('');
    expect(calls[0]).toEqual({
      name: 'setSecret',
      arg: {
        key: 'provider_instance.claude.env.ANTHROPIC_API_KEY',
        value: 'sk-live-123',
        revision: 'rev-1',
        elevation: 'el-9',
      },
    });
    // The outcome holds no value.
    expect(JSON.stringify(out)).not.toContain('sk-live-123');
  });

  test('without an elevation the input is left alone; the dialog asks first', async () => {
    let typed = 'secret-1';
    const { actions, calls } = fake({});
    const out = await relaySecret(actions, {
      station: 's',
      key: 'k',
      revision: 'r',
      read: () => typed,
      clear: () => {
        typed = '';
      },
    });
    expect(out).toEqual({ kind: 'elevate' });
    expect(typed).toBe('secret-1');
    expect(calls).toEqual([]);
  });

  test('a failure holds no value; an empty value is refused', async () => {
    let typed = 'sk-live-999';
    const { actions } = fake({
      elevation: 'el',
      setSecret: () => fail(409, 'settings_changed', 'the settings changed since you read them'),
    });
    const params = {
      station: 's',
      key: 'k',
      revision: 'r',
      read: () => typed,
      clear: () => {
        typed = '';
      },
    };
    const out = await relaySecret(actions, params);
    expect(out.kind).toBe('changed');
    expect(JSON.stringify(out)).not.toContain('sk-live-999');
    expect(await relaySecret(actions, params)).toEqual({ kind: 'empty' });
    const bad = fake({
      elevation: 'el',
      setSecret: () => fail(500, 'settings_write_failed', 'fixed text'),
    });
    typed = 'x';
    expect(await relaySecret(bad.actions, params)).toEqual({ kind: 'failed', text: 'fixed text' });
  });
});

describe('the restart flow (DL-172 7)', () => {
  const items = [
    { kind: 'session', id: 's1', title: 'Write the docs' },
    { kind: 'shell', id: 'sh1', title: '/work' },
  ] as const;

  test('it needs an elevation first', async () => {
    const { actions, calls } = fake({});
    expect(await tryRestart(actions, { kind: 'deck' }, false)).toEqual({ kind: 'elevate' });
    expect(calls).toEqual([]);
  });

  test('a refusal lists what stops; then it posts again with stop_direct_sessions', async () => {
    let stopped = false;
    const { actions, calls } = fake({
      elevation: 'el',
      restart: () =>
        stopped
          ? Promise.resolve({ restarting: true, would_stop: [] })
          : fail(409, 'restart_would_stop_sessions', 'a restart would end live work', {
              would_stop: items,
            }),
    });
    const first = await tryRestart(actions, { kind: 'station', id: 'st-alpha' }, false);
    expect(first).toEqual({ kind: 'would_stop', items });
    stopped = true;
    expect(await tryRestart(actions, { kind: 'station', id: 'st-alpha' }, true)).toEqual({
      kind: 'restarting',
    });
    expect(calls.map((c) => c.arg)).toEqual([
      { stop: false, elevation: 'el' },
      { stop: true, elevation: 'el' },
    ]);
  });

  test('the dialog lists the items with "Restart and stop N"', () => {
    const html = renderToStaticMarkup(
      <RestartDialog
        name="Alpha"
        view={{ phase: 'would_stop', items }}
        onConfirm={noop}
        onCancel={noop}
      />,
    );
    expect(html).toContain('Restart Alpha');
    expect(html).toContain('Write the docs');
    expect(html).toContain('/work');
    expect(html).toContain('fd-danger');
    expect(html).toContain('Restart and stop 2');
    const confirm = renderToStaticMarkup(
      <RestartDialog name="deck" view={{ phase: 'confirm' }} onConfirm={noop} onCancel={noop} />,
    );
    expect(confirm).toContain('>Restart</button>');
  });

  test('a station that cannot restart says so', async () => {
    const { actions } = fake({
      elevation: 'el',
      restart: () => fail(501, 'restart_unsupported', 'this process cannot restart itself yet'),
    });
    expect(await tryRestart(actions, { kind: 'station', id: 's' }, false)).toEqual({
      kind: 'unsupported',
    });
    expect(UNSUPPORTED_TEXT).toBe(
      'This station cannot restart itself. Restart it from its machine.',
    );
    const html = renderToStaticMarkup(
      <RestartDialog
        name="Alpha"
        view={{ phase: 'failed', text: UNSUPPORTED_TEXT }}
        onConfirm={noop}
        onCancel={noop}
      />,
    );
    expect(html).toContain('This station cannot restart itself. Restart it from its machine.');
  });

  test('a restart answer, then the overlay until the connection is live again', () => {
    const html = renderToStaticMarkup(<RestartOverlay name="deck" giveUp={false} onClose={noop} />);
    expect(html).toContain('Restarting…');
    expect(html).toContain('The board reconnects by itself.');
    expect(html).toContain('role="alert"');
    expect(renderToStaticMarkup(<RestartOverlay name="deck" giveUp onClose={noop} />)).toContain(
      'Close',
    );
    // Live at once, with no drop seen: wait for the grace time.
    expect(waitStep(false, true, 1_000)).toEqual({ sawDown: false, done: false, giveUp: false });
    // A drop, then live: done.
    expect(waitStep(false, false, 2_000)).toEqual({ sawDown: true, done: false, giveUp: false });
    expect(waitStep(true, true, 5_000)).toEqual({ sawDown: true, done: true, giveUp: false });
    // Too quick to see: done after the grace time.
    expect(waitStep(false, true, 12_000).done).toBe(true);
    // Never back: the owner is told to look.
    expect(waitStep(true, false, 95_000)).toEqual({ sawDown: true, done: false, giveUp: true });
  });
});

describe('the Settings link (DL-172)', () => {
  const env = {
    settings: fake({}).actions,
    accounts: {} as never,
    tailnet: {
      view: () => Promise.reject(new Error('not in this test')),
      use: () => Promise.reject(new Error('not in this test')),
      devices: () => Promise.reject(new Error('not in this test')),
      window: () => Promise.reject(new Error('not in this test')),
      setWindow: () => Promise.reject(new Error('not in this test')),
      requests: () => Promise.reject(new Error('not in this test')),
      approve: () => Promise.reject(new Error('not in this test')),
      deny: () => Promise.reject(new Error('not in this test')),
      reports: {
        list: () => Promise.reject(new Error('not in this test')),
        setWindow: () => Promise.reject(new Error('not in this test')),
        allowRetry: () => Promise.reject(new Error('not in this test')),
        cancelRetry: () => Promise.reject(new Error('not in this test')),
        dismiss: () => Promise.reject(new Error('not in this test')),
      },
    },
    displays: {} as never,
    desktop: {} as never,
    capture: {} as never,
    deletion: {} as never,
  };

  test('the sidebar and More show it for the owner login only', () => {
    const more = (withEnv: boolean) =>
      renderToStaticMarkup(
        withEnv ? (
          <SettingsProvider env={env}>
            <More terminalCount={0} />
          </SettingsProvider>
        ) : (
          <More terminalCount={0} />
        ),
      );
    expect(more(true)).toContain('href="#/settings"');
    expect(more(false)).not.toContain('href="#/settings"');
    const sidebar = (withEnv: boolean) =>
      renderToStaticMarkup(
        withEnv ? (
          <SettingsProvider env={env}>
            <Sidebar
              sessions={[]}
              stations={[]}
              route={{ name: 'home' }}
              terminalCount={0}
              onNavigate={noop}
              onOpenTerminals={noop}
            />
          </SettingsProvider>
        ) : (
          <Sidebar
            sessions={[]}
            stations={[]}
            route={{ name: 'home' }}
            terminalCount={0}
            onNavigate={noop}
            onOpenTerminals={noop}
          />
        ),
      );
    expect(sidebar(true)).toContain('href="#/settings"');
    expect(sidebar(false)).not.toContain('href="#/settings"');
  });
});
