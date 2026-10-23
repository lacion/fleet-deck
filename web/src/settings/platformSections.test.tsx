// The live UI audit round 3 of a station tab: the sections that a station shows by its system and
// by what it has. A macOS station has no Omarchy, no desktop tools and no Omarchy bar summary; a
// station without Omarchy and an empty tracker list show no section that only says so (§1.7).
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { SettingsActions } from './actions';
import type { DesktopToolsActions } from './desktopTools';
import { meta, view } from './fixtures';
import type { OmarchyStatus } from './omarchy';
import { OmarchyView } from './OmarchyCard';
import { fakeActions, journal } from './omarchyFixtures';
import type { RepairsState } from './OmarchyRepairPanels';
import { SettingsTab } from './SettingsTab';

const noop = () => undefined;

const settings = {
  view: async () => view(),
  elevation: () => null,
} as unknown as SettingsActions;

const desktopFields = [
  ...view().fields,
  meta('allow_desktop', 'Desktop control', 'bool', { danger: true }),
  meta('fleet_summary', 'Fleet summary for the desktop shell', 'bool', { danger: true }),
];

const tab = (
  os: string | undefined,
  extra: { readonly values?: Record<string, unknown>; readonly stationId?: string | null } = {},
) =>
  renderToStaticMarkup(
    <SettingsTab
      stationId={extra.stationId === undefined ? 'st-alpha' : extra.stationId}
      name="Alpha"
      actions={settings}
      live
      initial={{
        view: view({ fields: desktopFields, values: { ...view().values, ...extra.values } }),
      }}
      omarchy={fakeActions({})}
      desktopTools={{} as DesktopToolsActions}
      os={os}
    />,
  );

describe('the sections of a station by its system', () => {
  test('a macOS station: no Omarchy, no Desktop tools, no summary for the Omarchy bar', () => {
    const html = tab('macos');
    expect(html).toContain('aria-label="Desktop"');
    expect(html).not.toContain('aria-label="Omarchy"');
    expect(html).not.toContain('aria-label="Desktop tools"');
    expect(html).not.toContain('Fleet summary');
    expect(html).not.toContain('Omarchy bar');
  });

  test('a Linux station keeps the two cards and the summary switch', () => {
    for (const os of ['linux', 'Linux', 'omarchy']) {
      const html = tab(os);
      expect(html).toContain('aria-label="Omarchy"');
      expect(html).toContain('aria-label="Desktop tools"');
      expect(html).toContain('Fleet summary');
    }
  });

  test('a station that reports no system shows no Linux card', () => {
    const html = tab(undefined);
    expect(html).not.toContain('aria-label="Omarchy"');
    expect(html).not.toContain('aria-label="Desktop tools"');
  });
});

describe('the trackers of the station file', () => {
  test('an entry: the section, its table and the help that sends to Accounts', () => {
    const html = tab('linux');
    expect(html).toContain('aria-label="Trackers"');
    expect(html).toContain('>Trackers in the station file<');
    expect(html).toContain(
      'Read from the station file. Connect Jira and Linear accounts in the Accounts tab.',
    );
    expect(html).not.toContain('Edit the file');
  });

  test('no entry: no section (Jira and Linear connect in the Accounts tab)', () => {
    for (const trackers of [[], null, undefined]) {
      const html = tab('linux', { values: { trackers } });
      expect(html).not.toContain('aria-label="Trackers"');
      expect(html).not.toContain('No trackers');
    }
  });
});

const absent = (extra: Partial<OmarchyStatus> = {}): OmarchyStatus => ({
  omarchy: { present: false, version: null },
  install: null,
  screensaver: null,
  provision: null,
  running: null,
  ...extra,
});

const card = (
  status: OmarchyStatus,
  extra: { readonly repairs?: RepairsState; readonly busy?: boolean } = {},
) =>
  renderToStaticMarkup(
    <OmarchyView
      state={{ kind: 'ready', status }}
      busy={extra.busy ?? false}
      failure={null}
      onAct={noop}
      onRetryRead={noop}
      repairs={extra.repairs ?? { kind: 'ready', journals: [] }}
    />,
  );

describe('the Omarchy section of a station without Omarchy', () => {
  test('only the absence to say: no section', () => {
    expect(card(absent())).toBe('');
    expect(card(absent(), { repairs: { kind: 'idle' } })).toBe('');
  });

  test('a fact, a repair of before or a running action keeps the section', () => {
    const provisioning = card(
      absent({
        provision: { phase: 'link', error: null, bootstrap_running: true, last_retry: null },
      }),
    );
    expect(provisioning).toContain('aria-label="Omarchy"');
    expect(provisioning).toContain('Omarchy was not found on this station.');
    expect(card(absent(), { repairs: { kind: 'ready', journals: [journal()] } })).toContain(
      'aria-label="Omarchy"',
    );
    const busy = card(absent(), { busy: true });
    expect(busy).toContain('<p class="fd-note fd-loading" role="status">Working…</p>');
  });

  test('loading, an error and an older build keep their states', () => {
    const view = (state: Parameters<typeof OmarchyView>[0]['state']) =>
      renderToStaticMarkup(
        <OmarchyView state={state} busy={false} failure={null} onAct={noop} onRetryRead={noop} />,
      );
    expect(view({ kind: 'loading' })).toContain('aria-label="Omarchy"');
    expect(view({ kind: 'error' })).toContain('The Omarchy state could not be read.');
    expect(view({ kind: 'unsupported' })).toContain('aria-label="Omarchy"');
  });
});
