// BG-1: the calls, the validators and the pure texts of the desktop tools card.
import { describe, expect, test } from 'bun:test';
import type { DesktopToolsPlan, MaintenanceInfo, StationTools, ToolFact } from '../gen';
import {
  type DesktopToolsActions,
  desktopToolsActions,
  keyboardState,
  maintenanceTerminalActions,
  makeRequestIds,
  newRequestId,
  openErrorText,
  openInstall,
  planErrorText,
  planOf,
  planView,
  pluginState,
  postExitLines,
  runOf,
  runsOf,
  shortState,
  stateLines,
  toolName,
  toolRows,
  toolState,
  toolsOf,
  toolsSummary,
} from './desktopTools';
import { DeckError, type DeckHttp } from './http';
import { ELEVATION_HEADER } from './omarchy';

const ID = 'a'.repeat(32);

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
  sampled_at: 1_792_400_000_000,
  desktop: 'omarchy',
  tools: [fact()],
  keyboard: { status: 'ok', layouts: ['us', 'de'] },
  plugin: { status: 'valid' },
  passwordless_sudo: 'off',
  ...extra,
});

const plan = (extra: Partial<DesktopToolsPlan> = {}): DesktopToolsPlan => ({
  plan_id: 'p1',
  expires_at: 1_792_400_120_000,
  installable: ['wtype', 'wf_recorder'],
  unavailable: [],
  already_present: ['grim'],
  command_display: 'sudo pacman -S --needed --noconfirm extra/wtype extra/wf-recorder',
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

type Sent = { method: string; path: string; body?: unknown; headers?: unknown };

function io(reply: unknown) {
  const sent: Sent[] = [];
  const deck: DeckHttp = {
    get: async (path, headers) => {
      sent.push({ method: 'GET', path, headers });
      return reply;
    },
    post: async (path, body, headers) => {
      sent.push({ method: 'POST', path, body, headers });
      return reply;
    },
    del: async () => reply,
  };
  return { deck, sent };
}

describe('the validators', () => {
  test('accept the generated shapes', () => {
    expect(toolsOf(tools())).toEqual(tools());
    expect(planOf(plan())).toEqual(plan());
    expect(
      planOf(plan({ blocked: 'transaction_refused', plan_id: null, command_display: null }))
        .blocked,
    ).toBe('transaction_refused');
    expect(runOf(run())).toEqual(run());
    expect(
      runsOf({ runs: [run(), run({ state: 'exited', exit_code: 0, post_check: 'ok' })] }).runs,
    ).toHaveLength(2);
  });

  test('refuse broken shapes with the fixed text', () => {
    const text = 'The deck sent an invalid answer.';
    expect(() => toolsOf(null)).toThrow(text);
    expect(() => toolsOf({ ...tools(), desktop: 'kde' })).toThrow(text);
    expect(() => toolsOf({ ...tools(), tools: [{ ...fact(), probe: 'fine' }] })).toThrow(text);
    expect(() => toolsOf({ ...tools(), keyboard: { status: 'ok', layouts: [1] } })).toThrow(text);
    expect(() => toolsOf({ ...tools(), plugin: { status: 'x' } })).toThrow(text);
    expect(() => toolsOf({ ...tools(), passwordless_sudo: true })).toThrow(text);
    expect(() => planOf({ ...plan(), installable: 'wtype' })).toThrow(text);
    expect(() => planOf({ ...plan(), transaction: [{ repo: 'extra' }] })).toThrow(text);
    expect(() => planOf(plan({ plan_id: null }))).toThrow(text);
    expect(() => runOf({ ...run(), maintenance_id: 'ABC' })).toThrow(text);
    expect(() => runOf({ ...run(), state: 'done' })).toThrow(text);
    expect(() => runOf({ ...run(), post_check: 'x' })).toThrow(text);
    expect(() => runsOf({ runs: [{}] })).toThrow(text);
    expect(() => runsOf([])).toThrow(text);
  });
});

describe('desktopToolsActions', () => {
  test('tools and plan read the encoded station', async () => {
    const a = io(tools());
    await desktopToolsActions(a.deck).tools('my station/1');
    expect(a.sent).toEqual([
      { method: 'GET', path: '/api/stations/my%20station%2F1/desktop-tools', headers: undefined },
    ]);
    const b = io(plan());
    await desktopToolsActions(b.deck).plan('alpha');
    expect(b.sent[0]?.path).toBe('/api/stations/alpha/desktop-tools/plan');
  });

  test('open sends the body and the elevation header', async () => {
    const { deck, sent } = io(run());
    const got = await desktopToolsActions(deck).open('alpha', 'p1', 'r'.repeat(32), 100, 30, 'tok');
    expect(got.maintenance_id).toBe(ID);
    expect(sent).toEqual([
      {
        method: 'POST',
        path: '/api/stations/alpha/maintenance',
        body: { plan_id: 'p1', request_id: 'r'.repeat(32), cols: 100, rows: 30 },
        headers: { [ELEVATION_HEADER]: 'tok' },
      },
    ]);
  });

  test('list, resize and the socket path encode the IDs', async () => {
    const { deck, sent } = io({ runs: [] });
    const actions = desktopToolsActions(deck);
    expect((await actions.list('a b')).runs).toEqual([]);
    await actions.resize('a b', 'x/y', 80, 24);
    expect(sent[0]?.path).toBe('/api/stations/a%20b/maintenance');
    expect(sent[1]).toEqual({
      method: 'POST',
      path: '/api/stations/a%20b/maintenance/x%2Fy/resize',
      body: { cols: 80, rows: 24 },
      headers: undefined,
    });
    expect(actions.socketPath('a b', ID)).toBe(`/api/stations/a%20b/maintenance/${ID}/attach`);
  });

  test('an invalid answer is an error', async () => {
    const { deck } = io({ nope: 1 });
    await expect(desktopToolsActions(deck).tools('a')).rejects.toThrow('invalid answer');
    await expect(desktopToolsActions(deck).list('a')).rejects.toThrow('invalid answer');
  });
});

describe('the terminal adapter', () => {
  const calls: string[] = [];
  const base: DesktopToolsActions = {
    tools: async () => tools(),
    plan: async () => plan(),
    open: async () => run(),
    list: async () => ({ runs: [] }),
    resize: async (station, id, cols, rows) => {
      calls.push(`resize ${station} ${id} ${cols} ${rows}`);
    },
    socketPath: (station, id) => `/s/${station}/${id}`,
  };
  const adapter = maintenanceTerminalActions(base, 'alpha');

  test('socketPath and resize go to the maintenance calls', async () => {
    expect(adapter.socketPath('ignored', ID)).toBe(`/s/alpha/${ID}`);
    await adapter.resize('ignored', ID, 90, 20);
    expect(calls).toEqual([`resize alpha ${ID} 90 20`]);
  });

  test('paste is refused and every other member rejects', async () => {
    await expect(
      adapter.paste('alpha', { uploadId: 'u', ext: 'png', dataBase64: '' }),
    ).rejects.toThrow('Pasting images is not available in the installation terminal.');
    const other = 'Not available in the installation terminal.';
    await expect(adapter.list('alpha')).rejects.toThrow(other);
    await expect(adapter.open('alpha', '/', 1, 1)).rejects.toThrow(other);
    await expect(adapter.close('alpha', ID)).rejects.toThrow(other);
    await expect(adapter.handover('alpha', 's', 1, 1)).rejects.toThrow(other);
    await expect(adapter.release('alpha', ID)).rejects.toThrow(other);
  });
});

describe('the tool texts', () => {
  test('the display names of the nine tools', () => {
    const ids = [
      'wtype',
      'wf_recorder',
      'wlrctl',
      'grim',
      'busctl',
      'jq',
      'hyprland',
      'chromium',
      'notify_send',
    ];
    expect(ids.map(toolName)).toEqual([
      'wtype',
      'wf-recorder',
      'wlrctl',
      'grim',
      'busctl',
      'jq',
      'Hyprland',
      'Chromium',
      'notify-send',
    ]);
    expect(toolName('other')).toBe('other');
    expect(toolName('constructor')).toBe('constructor');
  });

  test('every probe state: installed, not installed, or the check; never "unknown"', () => {
    expect(toolState(fact({ version: null, version_source: 'none' }))).toBe('Installed');
    expect(toolState(fact({ present: false, version: null }))).toBe('Not installed');
    expect(toolState(fact({ probe: 'missing', present: false, version: null }))).toBe(
      'Not installed',
    );
    expect(toolState(fact({ probe: 'unknown', version: null }))).toBe('Installed');
    expect(toolState(fact({ probe: 'timeout', version: null }))).toBe('Check timed out');
    expect(toolState(fact({ probe: 'failed', version: null }))).toBe('Check failed');
  });

  test('the version follows an installed tool, with its source when it is the package', () => {
    expect(toolState(fact())).toBe('Installed · 0.4 (package)');
    expect(toolState(fact({ version_source: 'executable' }))).toBe('Installed · 0.4');
  });

  test('the keyboard and plugin states', () => {
    // A layout in words with its code; a code outside the known layouts stays as it is.
    expect(keyboardState({ status: 'ok', layouts: ['es', 'de'] })).toBe(
      'Spanish (es), German (de)',
    );
    expect(keyboardState({ status: 'ok', layouts: ['us', 'se', 'xx'] })).toBe(
      'American English (us), Swedish (se), xx',
    );
    expect(keyboardState({ status: 'unavailable', layouts: [] })).toBe('Not available');
    expect(keyboardState({ status: 'failed', layouts: [] })).toBe('Check failed');
    expect(pluginState('valid')).toBe('Installed');
    expect(pluginState('invalid')).toBe('Not valid');
    expect(pluginState('unavailable')).toBe('Not installed');
    expect(pluginState('unknown')).toBe('Check failed');
  });

  test('the rows: the tools in order, then the keyboard and the plugin, each with its glyph', () => {
    const rows = toolRows(
      tools({
        tools: [
          fact(),
          fact({ id: 'wf_recorder', present: false, version: null, probe: 'missing' }),
        ],
        plugin: { status: 'invalid' },
      }),
    );
    expect(rows.map((r) => [r.name, r.state, r.status, r.code])).toEqual([
      ['wtype', 'Installed · 0.4 (package)', 'online', true],
      ['wf-recorder', 'Not installed', 'idle', true],
      ['Keyboard layouts', 'American English (us), German (de)', 'online', false],
      ['Fleet plugin', 'Not valid', 'lost', false],
    ]);
    // An application is a product name (sans), not a command.
    const apps = toolRows(tools({ tools: [fact({ id: 'hyprland' }), fact({ id: 'jq' })] }));
    expect(apps.map((r) => [r.name, r.code]).slice(0, 2)).toEqual([
      ['Hyprland', false],
      ['jq', true],
    ]);
  });

  test('the summary counts the installed tools', () => {
    expect(
      toolsSummary(
        tools({
          tools: [
            fact(),
            fact({ id: 'jq', probe: 'unknown', version: null }),
            fact({ id: 'grim', present: false, version: null, probe: 'missing' }),
          ],
        }),
      ),
    ).toBe('2 of 3 tools installed.');
    expect(toolsSummary(tools({ tools: [fact()] }))).toBe('1 of 1 tool installed.');
  });
});

describe('the plan states', () => {
  test('blocked, with and without names', () => {
    const refused =
      'The package transaction needs packages that Fleet Deck does not allow. Nothing was installed.';
    const blocked = plan({
      blocked: 'transaction_refused',
      refused: ['evil', 'worse'],
      plan_id: null,
      command_display: null,
    });
    expect(planView(blocked)).toEqual({
      open: false,
      lines: [refused, 'Not allowed: evil, worse'],
      command: null,
      transaction: [],
    });
    expect(planView({ ...blocked, refused: [] }).lines).toEqual([refused]);
    expect(planView({ ...blocked, blocked: 'other' }).open).toBe(false);
  });

  test('nothing installable, with and without wlrctl', () => {
    const empty = plan({ installable: [], plan_id: null, command_display: null, transaction: [] });
    expect(planView(empty).lines).toEqual(['All installable desktop tools are present.']);
    expect(planView({ ...empty, unavailable: ['wlrctl'] }).lines).toEqual([
      'All installable desktop tools are present.',
      'Fleet Deck has no supported package source for wlrctl on this station.',
    ]);
    expect(planView(empty).open).toBe(false);
  });

  test('a normal plan', () => {
    const view = planView(plan({ unavailable: ['wlrctl'] }));
    expect(view.open).toBe(true);
    expect(view.lines).toEqual([
      'To install: wtype, wf-recorder',
      'Already present: grim',
      'Fleet Deck has no supported package source for wlrctl on this station.',
    ]);
    expect(view.command).toBe('sudo pacman -S --needed --noconfirm extra/wtype extra/wf-recorder');
    expect(view.transaction).toEqual(['extra/wtype 0.4-1']);
    expect(planView(plan({ already_present: [] })).lines).toEqual([
      'To install: wtype, wf-recorder',
    ]);
  });
});

describe('open', () => {
  const cannot: DesktopToolsActions = {
    tools: async () => tools(),
    plan: async () => plan(),
    open: async () => {
      throw new Error('must not be called');
    },
    list: async () => ({ runs: [] }),
    resize: async () => undefined,
    socketPath: () => '',
  };

  test('the elevation label, and null sends no request', async () => {
    const labels: (readonly string[] | undefined)[] = [];
    const out = await openInstall(
      cannot,
      async (l) => {
        labels.push(l);
        return null;
      },
      'alpha',
      'p1',
      'r',
    );
    expect(out).toEqual({ kind: 'cancelled' });
    expect(labels).toEqual([['Install desktop tools']]);
  });

  test('a token opens the run with 100 x 30', async () => {
    const seen: unknown[][] = [];
    const actions: DesktopToolsActions = {
      ...cannot,
      open: async (...args) => {
        seen.push(args);
        return run();
      },
    };
    const out = await openInstall(actions, async () => 'tok', 'alpha', 'p1', 'r1');
    expect(out).toEqual({ kind: 'opened', run: run() });
    expect(seen).toEqual([['alpha', 'p1', 'r1', 100, 30, 'tok']]);
  });

  test('each 409 code has its text', () => {
    const e = (status: number, code: string) => new DeckError(status, 'raw', code, {});
    expect(openErrorText(e(409, 'shell_disabled'))).toBe(
      'Board terminals are disabled on this station. Enable them in station settings to install desktop tools.',
    );
    expect(openErrorText(e(409, 'plan_changed'))).toBe(
      'The plan changed or expired. Review the desktop tools again.',
    );
    expect(openErrorText(e(409, 'maintenance_busy'))).toBe(
      'Another installation is running on this station.',
    );
    expect(openErrorText(e(409, 'transaction_refused'))).toBe(
      'The package transaction needs packages that Fleet Deck does not allow. Nothing was installed.',
    );
    expect(openErrorText(e(409, 'request_id_conflict'))).toBe(
      'This request was already used for another plan.',
    );
    expect(openErrorText(e(501, 'unsupported'))).toBe(
      "This station's build cannot install desktop tools from the board.",
    );
    expect(openErrorText(e(400, 'invalid_request'))).toBe('raw');
    expect(openErrorText(new Error('plain'))).toBe('plain');
  });

  test('a failed open is a failed outcome', async () => {
    const actions: DesktopToolsActions = {
      ...cannot,
      open: async () => {
        throw new DeckError(409, 'raw', 'maintenance_busy', {});
      },
    };
    expect(await openInstall(actions, async () => 'tok', 'a', 'p', 'r')).toEqual({
      kind: 'failed',
      text: 'Another installation is running on this station.',
    });
  });

  test('a plan read error: 501 text, else the deck message', () => {
    expect(planErrorText(new DeckError(501, 'raw', 'unsupported', {}))).toBe(
      "This station's build cannot install desktop tools from the board.",
    );
    expect(planErrorText(new DeckError(500, 'It broke', null, {}))).toBe('It broke');
  });

  test('the request ID: 32 hex, the same for one plan, new for another', () => {
    expect(newRequestId()).toMatch(/^[0-9a-f]{32}$/);
    let n = 0;
    const ids = makeRequestIds(() => `id${++n}`);
    expect(ids('p1')).toBe('id1');
    expect(ids('p1')).toBe('id1');
    expect(ids('p2')).toBe('id2');
  });
});

describe('the state texts', () => {
  test('state lines', () => {
    expect(stateLines(run({ state: 'admitted' }))).toEqual(['Starting…']);
    expect(stateLines(run())).toEqual([
      'Installing… Enter your station password in the terminal if sudo asks.',
    ]);
    expect(stateLines(run({ state: 'exited', exit_code: 0, post_check: 'ok' }))).toEqual([
      'The package transaction finished.',
    ]);
    expect(stateLines(run({ state: 'exited', exit_code: 1, post_check: 'ok' }))).toEqual([
      'The package transaction ended with code 1.',
    ]);
    expect(
      stateLines(run({ state: 'exited', exit_code: 0, post_check: 'transaction_changed' })),
    ).toEqual([
      'The package transaction finished.',
      'The installed packages differ from the reviewed plan.',
    ]);
    expect(stateLines(run({ state: 'exited', exit_code: 0, post_check: null }))).toEqual([
      'The package transaction finished. The result could not be checked.',
    ]);
    expect(
      stateLines(run({ state: 'exited', exit_code: null, post_check: 'transaction_changed' })),
    ).toEqual([
      'The package transaction ended without an exit code.',
      'The installed packages differ from the reviewed plan.',
    ]);
    expect(stateLines(run({ state: 'lost' }))).toEqual([
      'The installation runner ended without a result. Check the desktop tools again.',
    ]);
  });

  test('short states', () => {
    expect(shortState(run({ state: 'admitted' }))).toBe('Starting');
    expect(shortState(run())).toBe('Running');
    expect(shortState(run({ state: 'exited', exit_code: 0, post_check: 'ok' }))).toBe('Finished');
    expect(shortState(run({ state: 'exited', exit_code: 0, post_check: null }))).toBe('Finished');
    expect(shortState(run({ state: 'exited', exit_code: 2 }))).toBe('Ended with code 2');
    expect(shortState(run({ state: 'exited', exit_code: null }))).toBe('Ended');
    expect(shortState(run({ state: 'lost' }))).toBe('Runner lost');
  });

  test('the post-exit lines', () => {
    const after = tools({
      tools: [
        fact({ id: 'wtype', present: true }),
        fact({ id: 'wf_recorder', present: false, probe: 'missing' }),
      ],
    });
    expect(postExitLines(['wtype', 'wf_recorder', 'grim'], after)).toEqual([
      'wtype: Installed. Desktop compatibility has not been verified.',
      'The package transaction finished, but wf-recorder is still missing from the desktop PATH.',
    ]);
  });
});
