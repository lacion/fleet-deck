// BG-1: the views of the desktop tools card, the installation panel and the Installations list.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DesktopToolsPlan, MaintenanceInfo, StationTools, ToolFact } from '../gen';
import {
  DesktopToolsView,
  FactsView,
  gateDesktopTools,
  InstallationsView,
  type InstallState,
  InstallView,
  type RunsState,
  type ToolsState,
} from './DesktopToolsCard';
import type { SettingsActions } from './actions';
import type { DesktopToolsActions } from './desktopTools';
import { view as settingsView } from './fixtures';
import { lineTexts, MaintenanceView } from './MaintenancePanel';
import type { OmarchyActions } from './omarchy';
import { SettingsTab } from './SettingsTab';

const ID = 'a'.repeat(32);
const noop = () => undefined;

const fact = (extra: Partial<ToolFact> = {}): ToolFact => ({
  id: 'wtype',
  present: true,
  version: '0.4',
  version_source: 'package',
  package_matches_executable: true,
  probe: 'ok',
  ...extra,
});

const tools = (extra: Partial<StationTools> = {}): StationTools => ({
  sampled_at: 1,
  desktop: 'omarchy',
  tools: [fact(), fact({ id: 'wf_recorder', present: false, version: null, probe: 'missing' })],
  keyboard: { status: 'ok', layouts: ['us'] },
  plugin: { status: 'valid' },
  passwordless_sudo: 'off',
  ...extra,
});

const plan = (extra: Partial<DesktopToolsPlan> = {}): DesktopToolsPlan => ({
  plan_id: 'p1',
  expires_at: 2,
  installable: ['wtype'],
  unavailable: [],
  already_present: [],
  command_display: 'sudo pacman -S --needed --noconfirm extra/wtype',
  system_upgrade: false,
  blocked: null,
  refused: [],
  transaction: [{ repo: 'extra', name: 'wtype', version: '0.4-1' }],
  ...extra,
});

const run = (extra: Partial<MaintenanceInfo> = {}): MaintenanceInfo => ({
  maintenance_id: ID,
  request_id: 'b'.repeat(32),
  state: 'running',
  exit_code: null,
  post_check: null,
  created_at: 1_792_400_000_000,
  tools: ['wtype', 'wf_recorder'],
  ...extra,
});

const view = (
  state: ToolsState,
  extra: { runs?: RunsState; install?: InstallState | null; busy?: boolean } = {},
) =>
  renderToStaticMarkup(
    <DesktopToolsView
      tools={state}
      runs={extra.runs ?? { kind: 'ready', runs: [] }}
      install={extra.install ?? null}
      busy={extra.busy ?? false}
      onRefresh={noop}
      onInstall={noop}
      onOpenTerminal={noop}
      onCloseInstall={noop}
      onOpenRun={noop}
    />,
  );

const installView = (install: InstallState) =>
  renderToStaticMarkup(<InstallView state={install} onOpen={noop} onClose={noop} />);

describe('the card', () => {
  test('a 501 hides the whole card', () => {
    expect(view({ kind: 'hidden' })).toBe('');
  });

  test('loading, and a failed read with Refresh diagnostics', () => {
    const loading = view({ kind: 'loading' });
    expect(loading).toContain('aria-label="Desktop tools"');
    expect(loading).toContain('<h2>Desktop tools</h2>');
    expect(loading).toContain('Loading the desktop tools…');
    const error = view({ kind: 'error' });
    expect(error).toContain('role="alert"');
    expect(error).toContain('The desktop tools could not be read.');
    expect(error).toContain('Refresh diagnostics');
  });

  test('the tool rows, in the answer order, and both buttons', () => {
    const html = view({ kind: 'ready', tools: tools() });
    expect(html).toContain('wtype: Present · version 0.4 (package)');
    expect(html).toContain('wf-recorder: Missing');
    expect(html.indexOf('wtype: Present')).toBeLessThan(html.indexOf('wf-recorder: Missing'));
    expect(html).toContain('Keyboard layouts: us');
    expect(html).toContain('Fleet plugin: valid');
    expect(html).toContain('Refresh diagnostics');
    expect(html).toContain('Install desktop tools…');
  });

  test('buttons are off while a read runs', () => {
    expect(view({ kind: 'ready', tools: tools() }, { busy: true })).toContain('disabled');
  });

  test('the package line, the sudo note and the unknown plugin', () => {
    const html = renderToStaticMarkup(
      <FactsView
        tools={tools({
          tools: [fact({ package_matches_executable: false })],
          passwordless_sudo: 'on',
          plugin: { status: 'unavailable' },
          keyboard: { status: 'failed', layouts: [] },
        })}
      />,
    );
    expect(html).toContain('A different program with this name is used on the desktop.');
    expect(html).toContain('role="note"');
    expect(html).toContain('Passwordless sudo is enabled on this station.');
    expect(html).toContain('Fleet plugin: not installed');
    expect(html).toContain('Keyboard layouts could not be read.');
    const none = renderToStaticMarkup(<FactsView tools={tools()} />);
    expect(none).not.toContain('A different program');
    expect(none).not.toContain('Passwordless sudo');
  });
});

describe('the install panel', () => {
  test('reading', () => {
    const html = installView({ kind: 'reading' });
    expect(html).toContain('<h3>Install desktop tools</h3>');
    expect(html).toContain('Reading the install plan…');
  });

  test('an error shows its text and Close', () => {
    const html = installView({
      kind: 'error',
      text: "This station's build cannot install desktop tools from the board.",
    });
    expect(html).toContain('cannot install desktop tools from the board.');
    expect(html).toContain('>Close<');
    expect(html).not.toContain('Open installation terminal');
  });

  test('blocked: the names, Close, no open button', () => {
    const html = installView({
      kind: 'plan',
      plan: plan({
        blocked: 'transaction_refused',
        refused: ['evil', 'worse'],
        plan_id: null,
        command_display: null,
      }),
      notice: null,
      busy: false,
    });
    expect(html).toContain('Nothing was installed.');
    expect(html).toContain('Not allowed: evil, worse');
    expect(html).toContain('>Close<');
    expect(html).not.toContain('Open installation terminal');
  });

  test('nothing installable, and wlrctl', () => {
    const empty = plan({
      installable: [],
      unavailable: ['wlrctl'],
      plan_id: null,
      command_display: null,
    });
    const html = installView({ kind: 'plan', plan: empty, notice: null, busy: false });
    expect(html).toContain('All installable desktop tools are present.');
    expect(html).toContain(
      'Fleet Deck has no supported package source for wlrctl on this station.',
    );
    expect(html).toContain('>Close<');
  });

  test('a normal plan: the command, the transaction, the confirmation and both buttons', () => {
    const html = installView({
      kind: 'plan',
      plan: plan({ already_present: ['grim'] }),
      notice: null,
      busy: false,
    });
    expect(html).toContain('To install: wtype');
    expect(html).toContain('Already present: grim');
    expect(html).toContain('<pre>sudo pacman -S --needed --noconfirm extra/wtype</pre>');
    expect(html).toContain('Packages in the transaction:');
    expect(html).toContain('extra/wtype 0.4-1');
    expect(html).toContain(
      'This opens a board terminal to install the listed packages as root. Review the transaction and enter your station password there if sudo asks.',
    );
    expect(html).toContain('Open installation terminal');
    expect(html).toContain('>Cancel<');
    expect(html).not.toContain('>Close<');
  });

  test('a failed open shows its notice as an alert; busy turns the open button off', () => {
    const html = installView({
      kind: 'plan',
      plan: plan(),
      notice: 'Another installation is running on this station.',
      busy: true,
    });
    expect(html).toContain('role="alert"');
    expect(html).toContain('Another installation is running on this station.');
    expect(html).toMatch(/disabled=""[^>]*>Open installation terminal/);
  });
});

describe('the Installations list', () => {
  const list = (state: RunsState) =>
    renderToStaticMarkup(<InstallationsView state={state} onOpen={noop} />);

  test('a 501 hides it; loading; empty; a failed read', () => {
    expect(list({ kind: 'hidden' })).toBe('');
    expect(list({ kind: 'loading' })).toContain('Loading…');
    expect(list({ kind: 'ready', runs: [] })).toContain('No installations yet.');
    const error = list({ kind: 'error' });
    expect(error).toContain('role="alert"');
    expect(error).toContain('The installations could not be read.');
  });

  test('one row for each run, with its tools, state and button', () => {
    const html = list({
      kind: 'ready',
      runs: [
        run({ state: 'admitted' }),
        run({ maintenance_id: 'b'.repeat(32) }),
        run({ maintenance_id: 'c'.repeat(32), state: 'exited', exit_code: 0, post_check: 'ok' }),
        run({ maintenance_id: 'd'.repeat(32), state: 'exited', exit_code: 3 }),
        run({ maintenance_id: 'e'.repeat(32), state: 'lost' }),
      ],
    });
    expect(html).toContain('<h3>Installations</h3>');
    expect(html).toContain(new Date(1_792_400_000_000).toLocaleString());
    expect(html).toContain('wtype, wf-recorder');
    for (const text of ['Starting', 'Running', 'Finished', 'Ended with code 3', 'Runner lost']) {
      expect(html).toContain(`<span>${text}</span>`);
    }
    expect(html.match(/>Reattach</g)).toHaveLength(2);
    expect(html.match(/>Show output</g)).toHaveLength(3);
  });
});

describe('the installation panel', () => {
  const panel = (state: Parameters<typeof lineTexts>[0], post: string[] = []) =>
    renderToStaticMarkup(<MaintenanceView state={state} post={post} onDetach={noop} />);

  test('the title, a Detach button, and no close or stop control', () => {
    const html = panel({ kind: 'run', run: run() });
    expect(html).toContain('<h3>Installation terminal</h3>');
    expect(html).toContain('>Detach<');
    expect(html.match(/<button/g)).toHaveLength(1);
    expect(html).not.toMatch(/Close|Stop|Kill|Cancel/);
  });

  test('the state lines before the first answer and for a missing run', () => {
    expect(lineTexts({ kind: 'connecting' })).toEqual(['Connecting…']);
    expect(lineTexts({ kind: 'gone' })).toEqual(['This installation is no longer listed.']);
    expect(panel({ kind: 'connecting' })).toContain('Connecting…');
  });

  test('the post-exit lines show under the state line', () => {
    const html = panel(
      { kind: 'run', run: run({ state: 'exited', exit_code: 0, post_check: 'ok' }) },
      [
        'wtype: Installed. Desktop compatibility has not been verified.',
        'The package transaction finished, but wf-recorder is still missing from the desktop PATH.',
      ],
    );
    expect(html).toContain('The package transaction finished.');
    expect(html).toContain('wtype: Installed. Desktop compatibility has not been verified.');
    expect(html).toContain('wf-recorder is still missing from the desktop PATH.');
  });
});

describe('SettingsTab with the card', () => {
  const settings = {
    view: async () => settingsView(),
    elevation: () => null,
  } as unknown as SettingsActions;
  const omarchy = {} as OmarchyActions;
  const desktopTools = {} as DesktopToolsActions;
  const tab = (props: {
    live: boolean;
    omarchy?: OmarchyActions;
    desktopTools?: DesktopToolsActions;
    stationId?: string | null;
  }) =>
    renderToStaticMarkup(
      <SettingsTab
        stationId={props.stationId === undefined ? 'st-alpha' : props.stationId}
        name="Alpha"
        actions={settings}
        live={props.live}
        initial={{ view: settingsView() }}
        omarchy={props.omarchy}
        desktopTools={props.desktopTools}
      />,
    );

  test('a live station tab shows the card right after the Omarchy card', () => {
    const html = tab({ live: true, omarchy, desktopTools });
    expect(html).toContain('aria-label="Desktop tools"');
    expect(html.indexOf('aria-label="Omarchy"')).toBeLessThan(
      html.indexOf('aria-label="Desktop tools"'),
    );
  });

  test('no card when the station is not live, a call set is missing, or this is the deck tab', () => {
    const none = 'aria-label="Desktop tools"';
    expect(tab({ live: false, omarchy, desktopTools })).not.toContain(none);
    expect(tab({ live: true, omarchy })).not.toContain(none);
    expect(tab({ live: true, desktopTools })).not.toContain(none);
    expect(tab({ live: true, omarchy, desktopTools, stationId: null })).not.toContain(none);
  });
});

describe('the gate', () => {
  test('calls run one after the other; resize stays outside', async () => {
    const log: string[] = [];
    let release: () => void = () => undefined;
    const first = new Promise<void>((r) => {
      release = r;
    });
    const raw: DesktopToolsActions = {
      tools: async () => {
        log.push('tools start');
        await first;
        log.push('tools end');
        return tools();
      },
      plan: async () => {
        log.push('plan');
        return plan();
      },
      open: async () => run(),
      list: async () => ({ runs: [] }),
      resize: async () => {
        log.push('resize');
      },
      socketPath: () => '',
    };
    const gated = gateDesktopTools(raw, { current: Promise.resolve() });
    const a = gated.tools('s');
    const b = gated.plan('s');
    await new Promise<void>((r) => setTimeout(r, 0));
    await gated.resize('s', ID, 1, 1);
    expect(log).toEqual(['tools start', 'resize']);
    release();
    await Promise.all([a, b]);
    expect(log).toEqual(['tools start', 'resize', 'tools end', 'plan']);
  });

  test('a failed call does not stop the chain', async () => {
    const raw: DesktopToolsActions = {
      tools: async () => {
        throw new Error('no');
      },
      plan: async () => plan(),
      open: async () => run(),
      list: async () => ({ runs: [] }),
      resize: async () => undefined,
      socketPath: () => '',
    };
    const gated = gateDesktopTools(raw, { current: Promise.resolve() });
    await expect(gated.tools('s')).rejects.toThrow('no');
    expect((await gated.plan('s')).plan_id).toBe('p1');
  });
});
