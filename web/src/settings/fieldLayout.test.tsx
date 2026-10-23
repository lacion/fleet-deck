// The live UI audit round 2 of the settings fields: one place for the badges, one notation for the
// units, mono for paths and host names, the dependent desktop switches, one form of the help.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { MonitorInfo } from '../gen';
import { monitorLabel, monitorRaw } from '../ui/format';
import { fieldLabel, helpOf, unitOf } from './copy';
import { FieldEditor } from './FieldEditor';
import { meta, view } from './fixtures';
import { groupFields } from './model';
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

/** The markup of the field row whose label is `label`. */
const row = (html: string, label: string) => {
  const at = html.indexOf(label);
  const start = html.lastIndexOf('<div class="fd-fieldwrap"', at);
  const end = html.indexOf('<div class="fd-fieldwrap"', at);
  return html.slice(start, end < 0 ? undefined : end);
};

describe('the badges of a field have one place', () => {
  test('a one-value control: on the control line, after the control, hidden from the name', () => {
    const html = fields();
    const host = row(html, 'Where sessions run');
    expect(host).toMatch(
      /<span class="fd-set-ctlrow"><select[\s\S]*<\/select><span class="fd-set-badges" aria-hidden="true"><span class="fd-badge">Applies after a restart<\/span><\/span><\/span>/,
    );
    // The hint carries the words for assistive technology; the hint line has no badge.
    expect(host).toContain('<span class="fd-vh"> Applies after a restart.</span>');
    expect(host).not.toMatch(/fd-fieldhint">[\s\S]*fd-badge/);
  });

  test('a list: under the label text, in the label column', () => {
    const listen = row(fields(), 'Listen addresses');
    expect(listen).toContain(
      'class="fd-fieldlabel fd-set-labelbadges"><span class="fd-set-labeltext">Listen addresses',
    );
    expect(listen).toMatch(
      /fd-set-labeltext">[\s\S]*?<\/span><span class="fd-set-badges" aria-hidden="true"><span class="fd-badge">Applies after a restart<\/span><span class="fd-badge">Set by the command line \(--listen\)<\/span>/,
    );
  });

  test('a field without a badge keeps the plain markup', () => {
    expect(fields()).toMatch(/Most sessions<\/span><input/);
  });
});

describe('one notation for units', () => {
  test('the unit leaves the label and follows the field', () => {
    const buffer = meta('shim_buffer_mib', 'Output buffer (MiB)', 'int');
    expect(fieldLabel(buffer)).toBe('Output buffer');
    expect(unitOf(buffer)).toEqual({ short: 'MiB', words: 'MiB' });
    expect(
      fieldLabel(meta('worker_grant.idle_minutes', 'Worker grant idle time (minutes)', 'int')),
    ).toBe('Idle time');
    expect(fieldLabel(meta('security.login_idle_days', 'Login idle time (days)', 'int'))).toBe(
      'Login idle time',
    );
    expect(unitOf(meta('max_sessions', 'Most sessions', 'int'))).toBeNull();
    expect(unitOf(meta('x', 'A text (days)', 'text'))).toBeNull();
    const html = renderToStaticMarkup(
      <FieldEditor
        meta={meta('worker_grant.idle_minutes', 'Worker grant idle time (minutes)', 'int')}
        value={30}
        disabled={false}
        stationId={null}
        onChange={noop}
      />,
    );
    expect(html).toMatch(
      /<span class="fd-set-inline"><input type="number" inputMode="numeric" class="fd-set-unitnum" aria-label="Idle time in minutes"[^>]*\/><span class="fd-set-unit" aria-hidden="true">min<\/span><\/span>/,
    );
  });

  test('two fields that only the unit told apart keep two labels', () => {
    expect(fieldLabel(meta('limits.scrollback_lines', 'Terminal history (lines)', 'int'))).toBe(
      'Terminal history',
    );
    expect(fieldLabel(meta('limits.scrollback_mib', 'Terminal history (MiB)', 'int'))).toBe(
      'Terminal history size',
    );
  });
});

describe('the words of the fields', () => {
  test('a path or a host name is mono', () => {
    for (const key of ['tailscale_bin', 'tailnet_hostname']) {
      const html = renderToStaticMarkup(
        <FieldEditor
          meta={meta(key, key, 'text')}
          value="/mnt/c/Program Files/Tailscale/tailscale.exe"
          disabled={false}
          stationId={null}
          onChange={noop}
        />,
      );
      expect(html).toContain('data-mono="true"');
    }
    const name = renderToStaticMarkup(
      <FieldEditor
        meta={meta('name', 'Deck name', 'text')}
        value="deck"
        disabled={false}
        stationId={null}
        onChange={noop}
      />,
    );
    expect(name).not.toContain('data-mono');
  });

  test('one form of a default, and the deck name says what it is for', () => {
    expect(helpOf(meta('x', 'X', 'int', { help: 'The most rows. Default 200000.' }))).toBe(
      'The most rows. Default: 200,000.',
    );
    expect(helpOf(meta('x', 'X', 'int', { help: 'Default: the first root.' }))).toBe(
      'Default: the first root.',
    );
    const deckName = meta('name', 'Deck name', 'text', { help: 'The name of this deck.' });
    expect(helpOf(deckName, true)).toBe('The name that the board and the wall show for this deck.');
    expect(helpOf(meta('name', 'Station name', 'text', { help: 'Station help.' }))).toBe(
      'Station help.',
    );
    expect(helpOf(meta('allow_shell', 'Allow terminals', 'bool'))).toBe(
      'Lets the board open a terminal on this station.',
    );
  });

  test('the agent switches that need agent input follow it, indented', () => {
    const keys = [
      'allow_desktop_input_any',
      'allow_desktop_click',
      'allow_desktop_input',
      'allow_desktop',
    ];
    const desktop = groupFields(keys.map((k) => meta(k, k, 'bool')))[0];
    expect(desktop?.fields.map((f) => f.key)).toEqual([
      'allow_desktop',
      'allow_desktop_input',
      'allow_desktop_click',
      'allow_desktop_input_any',
    ]);
    const html = fields({
      view: view({ fields: keys.map((k) => meta(k, k, 'bool')), values: {}, overrides: [] }),
    });
    expect(html.match(/data-sub="true"/g)).toHaveLength(2);
    expect(html).toContain('Agent input in other windows');
  });
});

describe('the words of a monitor', () => {
  const monitor = (name: string, description: string): MonitorInfo => ({
    name,
    description,
    width: 1920,
    height: 1080,
    scale_pct: 100,
    refresh_hz: 60,
    focused: false,
    dpms_on: true,
  });

  test('a built-in panel is named so; hex model codes go; the title keeps the full text', () => {
    const panel = monitor('eDP-1', 'Samsung Display Corp. 0x41A1');
    expect(monitorLabel(panel)).toBe('Built-in display · eDP-1');
    expect(monitorRaw(panel)).toBe('eDP-1 — Samsung Display Corp. 0x41A1');
    expect(monitorLabel(monitor('DSI-1', ''))).toBe('Built-in display · DSI-1');
    const dell = monitor('DP-1', 'Dell Inc. DELL U2720Q 0x0001A2B3');
    expect(monitorLabel(dell)).toBe('Dell Inc. DELL U2720Q · DP-1');
    expect(monitorLabel(monitor('DP-2', 'Dell U2720Q'))).toBe('Dell U2720Q · DP-2');
  });

  test('the add button of the monitors says what it adds', () => {
    const html = fields({
      view: view({
        fields: [meta('capture_monitors', 'Screenshot monitors', 'texts')],
        values: { capture_monitors: [] },
        overrides: [],
      }),
    });
    expect(html).toContain('Add monitor</button>');
  });
});
