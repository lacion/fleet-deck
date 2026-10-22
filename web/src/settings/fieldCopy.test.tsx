// The marks of a field (badges, the password lock, the section note) and the board's own words for
// the registry (copy.ts): labels, help, option names, the bypass warning, the section slots.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { FIELDS, meta, view } from './fixtures';
import { badgesOf, labelOf, sectionId } from './model';
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

describe('the badges of a field', () => {
  test('restart and command line; a danger field has no badge', () => {
    const listen = FIELDS.find((f) => f.key === 'listen');
    expect(listen).toBeDefined();
    if (listen === undefined) return;
    expect(listen.danger).toBe(true);
    expect(badgesOf(listen, ['listen'])).toEqual([
      { text: 'Applies after a restart' },
      { text: 'Set by the command line (--listen)' },
    ]);
    expect(badgesOf(meta('name', 'Name', 'text'), [])).toEqual([]);
  });

  test('they are in the page, and a command line key is disabled', () => {
    const html = fields();
    expect(html).toContain('Applies after a restart');
    expect(html).toContain('Set by the command line (--listen)');
    expect(html).not.toContain('Needs re-auth');
    expect(html).toContain('aria-label="Listen addresses 1"');
    expect(html).toMatch(/<input[^>]*aria-label="Listen addresses 1" disabled=""/);
  });

  test('a danger field in a mixed section has a lock after its label and hidden words', () => {
    const html = fields();
    // Sessions: max_sessions (no danger) and host (danger).
    const sessions = /<section[^>]*aria-label="Sessions"[\s\S]*?<\/section>/.exec(html)?.[0] ?? '';
    expect(sessions).toMatch(
      /Where sessions run<span class="fd-set-lock" title="Saving asks for your password" aria-hidden="true">/,
    );
    expect(sessions).toContain('<span class="fd-vh"> Saving asks for your password.</span>');
    expect(sessions).not.toContain('fd-set-secnote');
    // The label of the control keeps its name: the lock is hidden from assistive technology.
    expect(sessions).toMatch(/Most sessions<\/span><input/);
  });

  test('a section whose fields all ask for the password says it once, next to its heading', () => {
    const grant = [
      meta('worker_grant.enabled', 'Worker grant', 'bool', { danger: true }),
      meta('worker_grant.spawn_cap', 'Worker grant session limit', 'int', { danger: true }),
    ];
    const html = fields({ view: view({ fields: grant, values: {} }), stationId: null });
    expect(html).toContain(
      '<h2 class="fd-set-sechead">Worker grants</h2><span class="fd-set-secnote">',
    );
    expect(html.split('Saving asks for your password').length - 1).toBe(1);
    expect(html).not.toContain('fd-set-lock');
    // The labels do not repeat the heading.
    expect(html).toContain('>Give new sessions a grant<');
    expect(html).toContain('>Sessions per worker<');
    expect(html).not.toContain('>Worker grant<');
  });
});

describe('the board copy of the fields', () => {
  test('the bypass switch warns while it is on, and its help has no raw mode names', () => {
    const bypass = meta('allow_bypass_permissions', 'Allow bypass permissions', 'bool', {
      help: 'Offer the mode that skips every permission check (Claude bypassPermissions, Codex danger-full-access) for sessions on this station.',
    });
    const on = fields({
      view: view({ fields: [bypass], values: { allow_bypass_permissions: true } }),
    });
    expect(on).toContain('<p class="fd-set-hazard"><svg');
    expect(on).toContain('Sessions on this station can run every tool without asking.');
    expect(on).toContain(
      'Offer the Bypass mode, which skips every permission check, for sessions on this station.',
    );
    expect(on).not.toContain('bypassPermissions');
    const off = fields({
      view: view({ fields: [bypass], values: { allow_bypass_permissions: false } }),
    });
    expect(off).not.toContain('fd-set-hazard');
  });

  test('the host help reads the same on every station version', () => {
    const old = meta('host', 'Where sessions run', 'enum', {
      options: ['direct', 'shim'],
      help: 'direct: sessions end when the station stops. shim: sessions keep running while the station restarts.',
    });
    const html = fields({ view: view({ fields: [old], values: { host: 'shim' } }) });
    expect(html).toContain(
      'Directly: sessions end when the station stops. In a shim: sessions keep running while the station restarts.',
    );
    expect(html).not.toContain('direct: sessions');
  });

  test('the empty option of host says the default of the station system', () => {
    const host = meta('host', 'Where sessions run', 'enum', { options: ['direct', 'shim'] });
    const at = (os: string | undefined) =>
      fields({ view: view({ fields: [host], values: { host: 'shim' } }), os });
    expect(at('linux')).toContain('<option value="">Default (In a shim)</option>');
    expect(at('macos')).toContain('<option value="">Default (Directly)</option>');
    expect(at(undefined)).toContain('<option value="">Default</option>');
  });

  test('the worker grant modes show words; the wire values stay', () => {
    const fieldsOf = [
      meta('worker_grant.claude_mode', 'Worker grant Claude mode', 'enum', {
        options: ['default', 'plan', 'acceptEdits', 'dontAsk', 'auto'],
        danger: true,
      }),
      meta('worker_grant.codex_mode', 'Worker grant Codex mode', 'enum', {
        options: ['untrusted/read-only', 'on-request/workspace-write', 'never/workspace-write'],
        danger: true,
      }),
      meta('clone_transport', 'Clone with', 'enum', { options: ['https', 'ssh'] }),
    ];
    const html = fields({
      view: view({
        fields: fieldsOf,
        values: {
          'worker_grant.claude_mode': 'acceptEdits',
          'worker_grant.codex_mode': 'on-request/workspace-write',
          clone_transport: 'ssh',
        },
      }),
      stationId: null,
    });
    expect(html).toContain('<option value="">Default (Accept edits)</option>');
    expect(html).toContain('<option value="default">Ask</option>');
    expect(html).toContain('<option value="acceptEdits" selected="">Accept edits</option>');
    expect(html).toContain('<option value="dontAsk">Don&#x27;t ask</option>');
    expect(html).toContain(
      '<option value="">Default (Ask when needed, write in the workspace)</option>',
    );
    expect(html).toContain(
      '<option value="untrusted/read-only">Ask for untrusted commands, read only</option>',
    );
    expect(html).toContain(
      '<option value="on-request/workspace-write" selected="">Ask when needed, write in the workspace</option>',
    );
    expect(html).toContain(
      '<option value="never/workspace-write">Never ask, write in the workspace</option>',
    );
    expect(html).toContain('<option value="">Default (HTTPS)</option>');
    expect(html).toContain('<option value="ssh" selected="">SSH</option>');
    expect(html).toContain('Default: Accept edits.');
    expect(html).toContain('Default: Ask when needed, write in the workspace.');
    expect(html).toContain('>Most permissive Claude mode<');
  });

  test('the label of a danger change in the confirmation is the label that the page shows', () => {
    const grant = [meta('worker_grant.spawn_cap', 'Worker grant session limit', 'int')];
    expect(labelOf(grant, 'worker_grant.spawn_cap')).toBe('Sessions per worker');
    expect(labelOf(FIELDS, 'max_sessions')).toBe('Most sessions');
    expect(labelOf(FIELDS, 'nope')).toBe('nope');
  });

  test('each section has an id for the section index', () => {
    expect(sectionId('Shells and setup')).toBe('fd-set-sec-shells-and-setup');
    expect(sectionId('Desktop tools')).toBe('fd-set-sec-desktop-tools');
    expect(fields()).toContain('id="fd-set-sec-general"');
  });

  test('a top card shows under its heading, an after part after its section', () => {
    const html = fields({
      slots: {
        Network: { top: <p>the network card</p> },
        Folders: { after: <p>after the folders</p> },
        Desktop: { after: <p>at the end</p> },
      },
    });
    const network = /<section[^>]*aria-label="Network"[\s\S]*?<\/section>/.exec(html)?.[0] ?? '';
    expect(network).toContain('</div><p>the network card</p><div class="fd-set-grid">');
    const folders = html.indexOf('aria-label="Folders"');
    const after = html.indexOf('<p>after the folders</p>');
    expect(folders).toBeLessThan(after);
    expect(after).toBeLessThan(html.indexOf('aria-label="Shells and setup"'));
    // No Desktop section in the view: its after part ends the page.
    expect(html.endsWith('<p>at the end</p>')).toBe(true);
  });
});
