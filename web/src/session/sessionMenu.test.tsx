// The session menu (its groups, the settings rows, the details) and the model picker of the
// composer chip; moved out of controls.test.tsx for the module size rule.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DeckSession, ModelChoice } from '../gen';
import { claudeCaps } from '../testing/capabilities';
import { card, station } from '../testing/fleet';
import { sessionActions } from './commands';
import { type Controller, sessionFlags } from './controller';
import { SessionHeader } from './SessionHeader';
import { ModelPicker, modelRowName, modelRows } from './SessionSettings';

const key = { station_id: 'station-a', session_id: 's1' };
const drive = sessionActions(async () => ({}), key, ['read', 'drive', 'approve']);

function session(
  extra: Partial<DeckSession['summary']> = {},
  status: DeckSession['status'] = 'working',
): DeckSession {
  return card('s1', status, 'station-a', {
    title: 'Fix login',
    capabilities: claudeCaps(),
    current_run: 'run-1',
    ...extra,
  });
}

const idle: Controller = {
  flags: sessionFlags(session(), 'live', drive),
  busy: null,
  outcome: null,
  act: () => {},
  dismiss: () => {},
};

function header(s: DeckSession, extra: { onNewShell?: () => void } = {}) {
  return renderToStaticMarkup(
    <SessionHeader
      session={s}
      stations={[station()]}
      controller={{ ...idle, flags: sessionFlags(s, 'live', drive) }}
      actions={drive}
      terminal={null}
      {...(extra.onNewShell ? { onNewShell: extra.onNewShell } : {})}
    />,
  );
}

describe('session menu (DL-140)', () => {
  test('the settings menu holds the mode and the auto-continue switch, not the model', () => {
    const html = header(session());
    expect(html).toContain('aria-label="Session menu"');
    expect(html).not.toContain('aria-label="Session settings"');
    // The order of the rows says "most restrictive first": the legend is the name only.
    expect(html).toContain('<legend>Mode</legend>');
    expect(html).not.toContain('most restrictive');
    // The modes are menu rows with their names; the title keeps the raw id.
    expect(html).toContain('<div class="fd-modes-list">');
    expect(html).not.toContain('class="fd-seg"');
    const names: readonly (readonly [string, string])[] = [
      ['default', 'Default'],
      ['plan', 'Plan'],
      ['acceptEdits', 'Accept edits'],
      ['dontAsk', "Don't ask"],
      ['auto', 'Auto'],
    ];
    for (const [id, name] of names) {
      expect(html).toMatch(
        new RegExp(`class="fd-mode-row fd-selbar" title="${id}" aria-pressed="(true|false)"`),
      );
      expect(html).toContain(`</span>${name.replace("'", '&#x27;')}`);
    }
    // A mode that skips the approvals has the shield-alert icon in its leading slot.
    expect(html).toMatch(/<span class="fd-mode-lead" data-risky="true"><svg[^>]*shield-alert/);
    // The desktop composer chip holds the model: the desktop menu has no model picker.
    expect(html).not.toContain('<legend>Model</legend>');
    expect(html).not.toContain('Set model');
    // A named switch row, the same as Compact cards and Reduce motion of the account menu.
    expect(html).toMatch(
      /<label class="fd-menu-switch"><span class="fd-menu-switch-label"><svg[^>]*>.*?<\/svg>Continue after a station restart<\/span><input type="checkbox" role="switch" class="fd-switch" aria-checked="false"/,
    );
    expect(html).not.toContain('fd-auto-continue');
  });

  test('the menu groups: the session, the work, the settings, then Stop, Force stop and Archive', () => {
    const html = header(session(), { onNewShell: () => {} });
    const menu = /aria-label="Session menu">[\s\S]*$/.exec(html)?.[0] ?? '';
    const at = (text: string) => menu.indexOf(text);
    const order = [
      ' Rename<',
      ' Star<',
      ' Ask for a status update<',
      ' Summarize<',
      ' Open shell in workspace<',
      '<legend>Mode</legend>',
      'Continue after a station restart',
      ' Stop<',
      ' Force stop…',
    ];
    for (const text of order) expect(at(text)).toBeGreaterThan(-1);
    for (let i = 1; i < order.length; i++) {
      expect(at(order[i] ?? '')).toBeGreaterThan(at(order[i - 1] ?? ''));
    }
    // A rule between two groups, none at the end.
    const between = (a: string, b: string) => menu.slice(at(a), at(b));
    expect(between(' Star<', ' Ask for a status update<')).toContain('fd-menu-sep');
    expect(between(' Open shell in workspace<', '<legend>Mode</legend>')).toContain('fd-menu-sep');
    expect(between('Continue after a station restart', ' Stop<')).toContain('fd-menu-sep');
    expect(between(' Rename<', ' Star<')).not.toContain('fd-menu-sep');
    expect(between(' Ask for a status update<', ' Open shell in workspace<')).not.toContain(
      'fd-menu-sep',
    );
  });

  test('with the Properties panel open, the menu has no details and no mode ladder', () => {
    const open = renderToStaticMarkup(
      <SessionHeader
        session={session({ session_seq: 42 })}
        stations={[station()]}
        controller={idle}
        actions={drive}
        terminal={null}
        propsPanel={{ open: true, toggle: () => {} }}
      />,
    );
    expect(open).not.toContain('class="fd-sh-details"');
    expect(open).not.toContain('<legend>Mode</legend>');
    expect(open).toContain('Continue after a station restart');
    const closed = renderToStaticMarkup(
      <SessionHeader
        session={session({ session_seq: 42 })}
        stations={[station()]}
        controller={idle}
        actions={drive}
        terminal={null}
        propsPanel={{ open: false, toggle: () => {} }}
      />,
    );
    expect(closed).toContain('class="fd-sh-details"');
    expect(closed).toContain('<legend>Mode</legend>');
  });

  test('the phone menu holds the model rows (the phone has no composer chip)', () => {
    const phone = renderToStaticMarkup(
      <SessionHeader
        session={session({ model: 'claude-opus-5-5[1m]' })}
        stations={[station()]}
        controller={idle}
        actions={drive}
        terminal={null}
        layout="phone"
        models={[{ value: 'opus', label: 'Opus', description: null }]}
      />,
    );
    expect(phone).toContain('<legend>Model</legend>');
    expect(phone).toContain('<span class="fd-model-name">Opus 5.5</span>');
    expect(phone).toContain('<legend>Mode</legend>');
  });

  test('the Files panel toggle shows a file tree, not "open the folder"', () => {
    const html = renderToStaticMarkup(
      <SessionHeader
        session={session()}
        stations={[station()]}
        controller={idle}
        actions={drive}
        terminal={null}
        filesPanel={{ open: false, toggle: () => {} }}
      />,
    );
    expect(html).toMatch(
      /aria-label="Files" title="Files" aria-pressed="false"><svg[^>]*folder-tree/,
    );
    expect(html).not.toContain('lucide-folder-open');
  });

  test('the details block lists the facts that the header does not show', () => {
    const html = header(
      session({
        callsign: 'falcon',
        worktree: '/wt/fix',
        session_seq: 42,
        updated_at: Date.now(),
        digest: '- Merged the branch\n- Ran the tests',
      }),
    );
    expect(html).toContain('class="fd-sh-details"');
    expect(html).toContain('<dt>Session</dt><dd><span class="fd-sh-sid" title="s1">s1</span>');
    expect(html).toContain('aria-label="Copy the session id"');
    // A path: mono (the span of the details) from its root, the full path in the title.
    expect(html).toContain('<dt>Worktree</dt><dd><span title="/wt/fix">/wt/fix</span></dd>');
    expect(
      header(session({ worktree: '/home/me/projects/wt' })).includes(
        '<span title="/home/me/projects/wt">~/projects/wt</span>',
      ),
    ).toBe(true);
    expect(html).toContain('<dt>Events</dt><dd>42</dd>');
    // The header shows the callsign (.fd-callsign) and the status says since when: no repeats.
    expect(html).not.toContain('<dt>Callsign</dt>');
    expect(html).not.toContain('<dt>Updated</dt>');
    // "Summary" and the 12px sparkles mark named "Generated", not a parenthesis.
    expect(html).toMatch(
      /<dt>Summary <span class="fd-generated" role="img" aria-label="Generated" title="Written by model help"><svg[^>]*width="12"/,
    );
    expect(html).not.toContain('(generated)');
    expect(html).toContain('<li>Merged the branch</li><li>Ran the tests</li>');
    // A big count has a thousands separator.
    expect(header(session({ session_seq: 7405 }))).toContain('<dt>Events</dt><dd>7,405</dd>');
    const bare = header(session());
    expect(bare).not.toContain('<dt>Worktree</dt>');
    expect(bare).not.toContain('<dt>Summary');
  });
});

describe('model picker (DL-151)', () => {
  const models: ModelChoice[] = [
    { value: 'default', label: 'Default (recommended)', description: 'The recommended model' },
    { value: 'opus', label: 'Opus', description: null },
    { value: 'sonnet[1m]', label: 'sonnet[1m]', description: null },
  ];
  const picker = (current: string | null, list: readonly ModelChoice[] | null, enabled = true) =>
    renderToStaticMarkup(
      <ModelPicker current={current} models={list} enabled={enabled} onSet={() => {}} />,
    );

  test('a list renders the models as menu rows and Other…', () => {
    const html = picker('opus', models);
    expect(html).not.toContain('<select');
    // The name only: no raw id and no "now" in the label.
    expect(html).toContain('<legend>Model</legend>');
    expect(html).not.toContain('(now');
    // The provider's label; a label that is only the raw id shows the short model name.
    expect(html).toContain('<span class="fd-model-name">Default (recommended)</span>');
    expect(html).toContain('title="default: The recommended model"');
    expect(html).toContain('<span class="fd-model-name">Opus</span>');
    // The context window is a fact of its own in --dim.
    expect(html).toContain(
      '<span class="fd-model-name">Sonnet</span><span class="fd-model-context">1M context</span>',
    );
    expect(html).toContain('aria-expanded="false">Other…</button>');
    // no text form until "Other…" is chosen
    expect(html).not.toContain('Set model');
  });

  test('the row of the current model is pressed and has a check', () => {
    const html = picker('opus', models);
    expect(html).toMatch(
      /class="fd-mode-row fd-model-row fd-selbar" title="opus" aria-pressed="true" disabled=""><span class="fd-model-name">Opus<\/span><svg[^>]*lucide-check/,
    );
    expect(html.match(/aria-pressed="true"/g)?.length).toBe(1);
  });

  test('a current id that matches no value gets a row of its own, first and current', () => {
    const html = picker('claude-opus-5-5[1m]', models);
    expect(html).not.toContain('Choose a model');
    expect(html).toMatch(
      /<div class="fd-modes-list"><button type="button" class="fd-mode-row fd-model-row fd-selbar" title="claude-opus-5-5\[1m\]" aria-pressed="true" disabled=""><span class="fd-model-name">Opus 5.5<\/span><span class="fd-model-context">1M context<\/span>/,
    );
    expect(html).not.toContain('>claude-opus-5-5[1m]<');
  });

  test('without models the free-text form stays, after the row of the current model', () => {
    for (const list of [null, []]) {
      const html = picker(null, list);
      expect(html).not.toContain('<select');
      expect(html).not.toContain('unknown');
      expect(html).toContain('placeholder="Model id or alias"');
      expect(html).toContain('aria-label="Model id or alias"');
      expect(html).toContain('Set model');
      expect(html).not.toContain('Other…');
    }
    expect(picker('claude-haiku-4-5', null)).toContain(
      '<span class="fd-model-name">Haiku 4.5</span>',
    );
  });

  test('a disabled picker disables the rows and the field', () => {
    expect(picker('opus', models, false)).toMatch(
      /title="sonnet\[1m\]" aria-pressed="false" disabled=""/,
    );
    expect(picker('opus', null, false)).toMatch(/<input[^>]*disabled/);
  });

  test('a row name: the label of the provider, or the short name when the label is the raw id', () => {
    expect(modelRowName('opus', 'Opus')).toBe('Opus');
    expect(modelRowName('claude-sonnet-5-5', 'claude-sonnet-5-5')).toBe('Sonnet 5.5');
    expect(modelRowName('claude-sonnet-5-5', 'Sonnet 5.5 (preview)')).toBe('Sonnet 5.5 (preview)');
    expect(modelRowName('default', 'Default (recommended)')).toBe('Default (recommended)');
    expect(modelRowName('gpt-6.1-codex', 'gpt-6.1-codex')).toBe('GPT-6.1 Codex');
    expect(modelRowName('custom-x', 'custom-x')).toBe('custom-x');
  });

  test('two rows with one name show their ids; a name that says the context has no aside', () => {
    const rows = modelRows([
      { value: 'claude-fable-5-6', label: 'claude-fable-5-6', description: null },
      { value: 'claude-fable-5-6-preview', label: 'claude-fable-5-6-preview', description: null },
      { value: 'opus[1m]', label: 'Opus (1M context)', description: null },
    ]);
    expect(rows.map((r) => [r.name, r.context, r.id])).toEqual([
      ['Fable 5.6', null, 'claude-fable-5-6'],
      ['Fable 5.6', null, 'claude-fable-5-6-preview'],
      ['Opus (1M context)', null, null],
    ]);
    const html = picker('opus', [
      { value: 'claude-fable-5-6', label: 'claude-fable-5-6', description: null },
      { value: 'claude-fable-5-6-x', label: 'claude-fable-5-6-x', description: null },
    ]);
    expect(html).toContain(
      '<span class="fd-model-name">Fable 5.6</span><code class="fd-model-id">claude-fable-5-6</code>',
    );
  });
});
