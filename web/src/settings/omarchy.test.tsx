import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { SettingsActions } from './actions';
import { DeckError, type DeckIo } from './http';
import {
  actionLabel,
  buttons,
  type CardState,
  cardLines,
  ELEVATION_HEADER,
  errorText,
  type OmarchyActions,
  type OmarchyActResult,
  type OmarchyStatus,
  omarchyActions,
  POLL_LIMIT_MS,
  POLL_MS,
  pollRetry,
  retryOver,
  runAction,
  stateOfReadError,
} from './omarchy';
import { OmarchyView } from './OmarchyCard';
import { SettingsTab } from './SettingsTab';
import { view } from './fixtures';

const status = (extra: Partial<OmarchyStatus> = {}): OmarchyStatus => ({
  omarchy: { present: true, version: '3.1.0' },
  install: {
    installed: true,
    files_ok: 5,
    files_changed: 0,
    files_missing: 0,
    plugin_enabled: true,
    hyprland_line: true,
    outdated: false,
  },
  screensaver: { enabled: true, file_restored: null, blocked: null },
  provision: { phase: 'complete', error: null, bootstrap_running: false, last_retry: null },
  running: null,
  ...extra,
});

const withInstall = (extra: Partial<NonNullable<OmarchyStatus['install']>>): OmarchyStatus => {
  const base = status();
  return {
    ...base,
    install: { ...(base.install as NonNullable<OmarchyStatus['install']>), ...extra },
  };
};

const withScreensaver = (
  extra: Partial<NonNullable<OmarchyStatus['screensaver']>>,
): OmarchyStatus => {
  const base = status();
  return {
    ...base,
    screensaver: {
      ...(base.screensaver as NonNullable<OmarchyStatus['screensaver']>),
      ...extra,
    },
  };
};

const withProvision = (extra: Partial<NonNullable<OmarchyStatus['provision']>>): OmarchyStatus => {
  const base = status();
  return {
    ...base,
    provision: { ...(base.provision as NonNullable<OmarchyStatus['provision']>), ...extra },
  };
};

const labels = (s: OmarchyStatus) => buttons(s).map((b) => b.label);

describe('cardLines', () => {
  test('a healthy station', () => {
    expect(cardLines(status())).toEqual([
      'Omarchy 3.1.0',
      'Integration: installed',
      'Plugin: on',
      'Hyprland line: present',
      'Screensaver: on',
      'Provisioning: complete',
    ]);
  });

  test('Omarchy absent: only its line, the screensaver and provisioning', () => {
    const s = status({
      omarchy: { present: false, version: null },
      install: null,
      screensaver: null,
      provision: { phase: 'link', error: null, bootstrap_running: true, last_retry: null },
    });
    expect(cardLines(s)).toEqual([
      'Omarchy was not found on this station.',
      'Provisioning: link',
      'Provisioning is running',
    ]);
  });

  test('the version is unknown', () => {
    expect(cardLines(status({ omarchy: { present: true, version: null } }))[0]).toBe(
      'Omarchy (version unknown)',
    );
  });

  test('not installed', () => {
    const lines = cardLines(
      withInstall({ installed: false, plugin_enabled: false, hyprland_line: false }),
    );
    expect(lines).toContain('Integration: not installed');
    expect(lines).toContain('Plugin: off');
    expect(lines).toContain('Hyprland line: missing');
  });

  test('changed and missing files add up', () => {
    expect(cardLines(withInstall({ files_changed: 2, files_missing: 1 }))).toContain(
      'Integration: installed, 3 files changed or missing',
    );
  });

  test('outdated adds the update line after the install lines', () => {
    const lines = cardLines(withInstall({ outdated: true }));
    expect(lines[4]).toBe('Update the integration first: press Install, then wait two minutes.');
  });

  test('each screensaver state', () => {
    expect(cardLines(withScreensaver({ enabled: true }))).toContain('Screensaver: on');
    expect(cardLines(withScreensaver({ enabled: true, blocked: 'differs' }))).toContain(
      'Screensaver: on, but blocked: the original screensaver that Fleet Deck kept differs from the current file.',
    );
    expect(cardLines(withScreensaver({ enabled: false, file_restored: false }))).toContain(
      'Screensaver: off. The screensaver file changed after Fleet Deck restored it; the original is kept as a backup.',
    );
    expect(cardLines(withScreensaver({ enabled: false, file_restored: true }))).toContain(
      'Screensaver: off',
    );
    expect(cardLines(withScreensaver({ enabled: false, file_restored: null }))).toContain(
      'Screensaver: off',
    );
    expect(cardLines(status({ screensaver: null })).some((l) => l.startsWith('Screensaver'))).toBe(
      false,
    );
  });

  test('each provisioning state', () => {
    const p = (extra: Partial<NonNullable<OmarchyStatus['provision']>>) =>
      cardLines(withProvision(extra)).slice(5);
    expect(p({ phase: null })).toEqual(['Provisioning: unknown', 'Provisioning did not finish']);
    expect(p({ phase: 'paired' })).toEqual(['Provisioning: paired', 'Provisioning did not finish']);
    expect(p({ phase: 'link', bootstrap_running: true })).toEqual([
      'Provisioning: link',
      'Provisioning is running',
    ]);
    expect(p({ phase: 'integration', error: 'paired_elsewhere' })).toEqual([
      'Provisioning: integration',
      'Provisioning stopped: The station is already paired with a deck that has another fingerprint.',
    ]);
    expect(p({ phase: 'link', error: 'something_new' })).toEqual([
      'Provisioning: link',
      'Provisioning stopped: something_new.',
    ]);
    expect(cardLines(status({ provision: null })).some((l) => l.startsWith('Provisioning'))).toBe(
      false,
    );
  });

  test('the owner text of every terminal code is the cause cell of operations.md 15.3', () => {
    const text = (code: string) =>
      cardLines(withProvision({ phase: 'link', error: code })).at(-1) as string;
    expect(text('station_identity_changed')).toBe(
      'Provisioning stopped: The station key is not the key that the bootstrap saved first.',
    );
    expect(text('config_changed')).toBe(
      'Provisioning stopped: The provisioning file changed after the pairing started.',
    );
    expect(text('release_untrusted')).toContain('is not owned by root');
    expect(text('build_mismatch')).toContain('does not match the trusted manifest.');
    expect(text('service_conflict')).toContain('another data folder.');
    expect(text('provider_probe_failed')).toContain('`broken` or `unknown`');
    expect(text('integration_failed')).toBe(
      'Provisioning stopped: The Omarchy integration install or its status check did not succeed.',
    );
    // A name of the Object prototype is an unknown code, not a text.
    expect(text('constructor')).toBe('Provisioning stopped: constructor.');
  });

  test('the last retry', () => {
    const last = (value: string) =>
      cardLines(withProvision({ last_retry: value })).at(-1) as string;
    expect(last('running')).toBe('Last retry: running');
    expect(last('complete')).toBe('Last retry: complete');
    expect(last('terminal')).toBe('Last retry: stopped with an error');
    expect(last('timeout')).toBe('Last retry: timed out');
    expect(last('failed')).toBe('Last retry: failed');
    expect(last('other')).toBe('Last retry: unknown');
  });

  test('an action that runs adds Working… as the last line', () => {
    expect(cardLines(status({ running: 'install' })).at(-1)).toBe('Working…');
  });
});

describe('buttons', () => {
  test('a healthy station with the screensaver on', () => {
    expect(labels(status())).toEqual(['Uninstall', 'Turn the screensaver off']);
  });

  test('the screensaver off', () => {
    expect(labels(withScreensaver({ enabled: false, file_restored: true }))).toEqual([
      'Uninstall',
      'Turn the screensaver on',
    ]);
  });

  test('the screensaver off and the restored file changed: no off button', () => {
    expect(labels(withScreensaver({ enabled: false, file_restored: false }))).toEqual([
      'Uninstall',
      'Turn the screensaver on',
    ]);
  });

  test('blocked: the rebaseline button', () => {
    expect(labels(withScreensaver({ enabled: true, blocked: 'differs' }))).toEqual([
      'Uninstall',
      'Turn the screensaver off',
      'Use the current screensaver as the original',
    ]);
  });

  test('not installed: Install only', () => {
    expect(labels(withInstall({ installed: false }))).toEqual(['Install']);
  });

  test('changed or missing files: Install and Uninstall', () => {
    expect(labels(withInstall({ files_changed: 1 }))).toEqual([
      'Install',
      'Uninstall',
      'Turn the screensaver off',
    ]);
    expect(labels(withInstall({ files_missing: 1 }))[0]).toBe('Install');
  });

  test('outdated: Install only', () => {
    expect(labels(withInstall({ outdated: true }))).toEqual(['Install']);
  });

  test('Omarchy absent: no install or screensaver button', () => {
    expect(labels(status({ omarchy: { present: false, version: null }, install: null }))).toEqual(
      [],
    );
  });

  test('retry provisioning', () => {
    const retry = (extra: Partial<NonNullable<OmarchyStatus['provision']>>) =>
      labels(withProvision(extra)).includes('Retry provisioning');
    expect(retry({ phase: 'paired', error: 'config_changed' })).toBe(true);
    expect(retry({ phase: 'link', error: 'config_changed', bootstrap_running: true })).toBe(true);
    expect(retry({ phase: 'integration' })).toBe(true);
    expect(retry({ phase: 'link', bootstrap_running: true })).toBe(false);
    expect(retry({ phase: 'complete' })).toBe(false);
    expect(retry({ phase: 'complete', error: 'config_changed' })).toBe(false);
    expect(retry({ phase: null })).toBe(false);
    expect(retry({ phase: 'preparing' })).toBe(false);
    expect(labels(status({ provision: null })).includes('Retry provisioning')).toBe(false);
  });

  test('an action that runs: no button', () => {
    expect(buttons(status({ running: 'install' }))).toEqual([]);
  });

  test('the label of each action', () => {
    expect(actionLabel('install')).toBe('Install');
    expect(actionLabel('uninstall')).toBe('Uninstall');
    expect(actionLabel('screensaver_on')).toBe('Turn the screensaver on');
    expect(actionLabel('screensaver_off')).toBe('Turn the screensaver off');
    expect(actionLabel('screensaver_rebaseline')).toBe(
      'Use the current screensaver as the original',
    );
    expect(actionLabel('provision_retry')).toBe('Retry provisioning');
  });
});

describe('errorText', () => {
  test('the codes of a failed answer', () => {
    expect(errorText('install_refused')).toBe('The installer refused: some files were changed.');
    expect(errorText('uninstall_incomplete')).toBe('The uninstall did not finish.');
    expect(errorText('screensaver_refused')).toBe('The screensaver change was refused.');
    expect(errorText('integration_outdated')).toBe(
      'Update the integration first: press Install, then wait two minutes.',
    );
    expect(errorText('timeout')).toBe('The station did not finish in time.');
    expect(errorText('output_too_large')).toBe('The command printed too much output.');
    expect(errorText('failed')).toBe('The command failed.');
  });

  test('the HTTP errors', () => {
    expect(errorText('elevation_required')).toBe('This action needs your confirmation.');
    expect(errorText('busy')).toBe('Another Omarchy action is running on this station.');
    expect(errorText('omarchy_missing')).toBe('Omarchy was not found on this station.');
    expect(errorText('not_provisioned')).toBe(
      'Provisioning cannot be retried from the board in this state.',
    );
    expect(errorText('unsupported')).toBe(
      "This station's build cannot manage Omarchy from the board.",
    );
  });

  test('any other code', () => {
    expect(errorText('station_error')).toBe('The action failed.');
    expect(errorText(null)).toBe('The action failed.');
    expect(errorText('constructor')).toBe('The action failed.');
  });
});

/** Fake calls: each `act` answers with the next reply; `calls` records what was sent. */
function fake(opts: { act?: () => Promise<OmarchyActResult>; statuses?: OmarchyStatus[] }) {
  const calls: { name: string; arg: unknown }[] = [];
  const queue = [...(opts.statuses ?? [])];
  const actions: OmarchyActions = {
    status: async (station) => {
      calls.push({ name: 'status', arg: station });
      const next = queue.shift();
      if (next === undefined) throw new Error('down');
      return next;
    },
    act: async (station, action, elevation) => {
      calls.push({ name: 'act', arg: { station, action, elevation } });
      return (
        opts.act ?? (async () => ({ outcome: 'done', code: null, message: null, status: status() }))
      )();
    },
  };
  return { actions, calls };
}

const result = (extra: Partial<OmarchyActResult> = {}): OmarchyActResult => ({
  outcome: 'done',
  code: null,
  message: null,
  status: status(),
  ...extra,
});

describe('runAction', () => {
  test('a cancelled confirmation sends nothing', async () => {
    const { actions, calls } = fake({});
    const end = await runAction(actions, 'alpha', 'install', async () => null);
    expect(end).toEqual({ kind: 'cancelled' });
    expect(calls).toEqual([]);
  });

  test('the elevation is asked with the label Omarchy integration and sent with the action', async () => {
    const { actions, calls } = fake({});
    const asked: (readonly string[] | undefined)[] = [];
    await runAction(actions, 'alpha', 'uninstall', async (l) => {
      asked.push(l);
      return 'tok';
    });
    expect(asked).toEqual([['Omarchy integration']]);
    expect(calls).toEqual([
      { name: 'act', arg: { station: 'alpha', action: 'uninstall', elevation: 'tok' } },
    ]);
  });

  test('done brings the new state', async () => {
    const next = status({ screensaver: null });
    const { actions } = fake({ act: async () => result({ status: next }) });
    expect(await runAction(actions, 'a', 'install', async () => 't')).toEqual({
      kind: 'done',
      status: next,
    });
  });

  test('failed shows the code text and the message', async () => {
    const { actions } = fake({
      act: async () =>
        result({ outcome: 'failed', code: 'install_refused', message: '<b>x</b>\nline 2' }),
    });
    const end = await runAction(actions, 'a', 'install', async () => 't');
    expect(end).toMatchObject({
      kind: 'failed',
      text: 'The installer refused: some files were changed.',
      message: '<b>x</b>\nline 2',
    });
  });

  test('failed without a code reads as the code failed', async () => {
    const { actions } = fake({ act: async () => result({ outcome: 'failed' }) });
    const end = await runAction(actions, 'a', 'install', async () => 't');
    expect(end).toMatchObject({ kind: 'failed', text: 'The command failed.', message: null });
  });

  test('an HTTP error shows its fixed text', async () => {
    const { actions } = fake({
      act: async () => {
        throw new DeckError(409, 'raw text of the deck', 'busy', {});
      },
    });
    const end = await runAction(actions, 'a', 'install', async () => 't');
    expect(end).toEqual({
      kind: 'failed',
      text: 'Another Omarchy action is running on this station.',
      message: null,
      status: null,
    });
  });

  test('a network error shows the generic text', async () => {
    const { actions } = fake({
      act: async () => {
        throw new Error('boom');
      },
    });
    const end = await runAction(actions, 'a', 'install', async () => 't');
    expect(end).toMatchObject({ kind: 'failed', text: 'The action failed.' });
  });

  test('started brings the status of the answer', async () => {
    const next = withProvision({ bootstrap_running: true, last_retry: 'running' });
    const { actions } = fake({ act: async () => result({ outcome: 'started', status: next }) });
    expect(await runAction(actions, 'a', 'provision_retry', async () => 't')).toEqual({
      kind: 'started',
      status: next,
    });
  });
});

const running = withProvision({ phase: 'link', bootstrap_running: true, last_retry: 'running' });
const finished = withProvision({
  phase: 'complete',
  bootstrap_running: false,
  last_retry: 'complete',
});

describe('pollRetry', () => {
  test('retryOver', () => {
    expect(retryOver(running)).toBe(false);
    expect(retryOver(withProvision({ bootstrap_running: false, last_retry: 'running' }))).toBe(
      false,
    );
    expect(retryOver(withProvision({ bootstrap_running: true, last_retry: 'complete' }))).toBe(
      false,
    );
    expect(retryOver(finished)).toBe(true);
    expect(retryOver(status({ provision: null }))).toBe(true);
  });

  /** A clock that moves only when the poll sleeps. */
  const clock = () => {
    let t = 0;
    const slept: number[] = [];
    return {
      slept,
      sleep: async (ms: number) => {
        slept.push(ms);
        t += ms;
      },
      now: () => t,
    };
  };

  test('it polls every 5 s and stops when the retry is over', async () => {
    const { actions, calls } = fake({ statuses: [running, running, finished] });
    const c = clock();
    const seen: OmarchyStatus[] = [];
    await pollRetry(actions, 'alpha', {
      ...c,
      stopped: () => false,
      onStatus: (s) => seen.push(s),
    });
    expect(c.slept).toEqual([POLL_MS, POLL_MS, POLL_MS]);
    expect(seen).toEqual([running, running, finished]);
    expect(calls.map((x) => x.arg)).toEqual(['alpha', 'alpha', 'alpha']);
  });

  test('it stops after 10 min, also when the reads fail', async () => {
    const { actions, calls } = fake({ statuses: [] });
    const c = clock();
    await pollRetry(actions, 'alpha', { ...c, stopped: () => false, onStatus: () => {} });
    expect(calls.length).toBe(POLL_LIMIT_MS / POLL_MS);
    expect(c.now()).toBe(POLL_LIMIT_MS);
  });

  test('it stops when the card is gone', async () => {
    const { actions, calls } = fake({ statuses: [running, running] });
    const c = clock();
    let gone = false;
    await pollRetry(actions, 'alpha', {
      ...c,
      stopped: () => gone,
      onStatus: () => {
        gone = true;
      },
    });
    expect(calls.length).toBe(1);
  });
});

describe('omarchyActions', () => {
  const io = (reply: unknown) => {
    const sent: { method: string; path: string; body?: unknown; headers?: unknown }[] = [];
    const deck: DeckIo = {
      get: async (path, headers) => {
        sent.push({ method: 'GET', path, headers });
        return reply;
      },
      post: async (path, body, headers) => {
        sent.push({ method: 'POST', path, body, headers });
        return reply;
      },
    };
    return { deck, sent };
  };

  test('status reads the station by its encoded name', async () => {
    const { deck, sent } = io(status());
    const got = await omarchyActions(deck).status('my station/1');
    expect(sent).toEqual([
      { method: 'GET', path: '/api/stations/my%20station%2F1/omarchy', headers: undefined },
    ]);
    expect(got.omarchy.version).toBe('3.1.0');
  });

  test('a missing optional part of a status reads as null', async () => {
    const { deck } = io({ omarchy: { present: false, version: null } });
    const got = await omarchyActions(deck).status('a');
    expect(got.install).toBeNull();
    expect(got.running).toBeNull();
  });

  test('an invalid status is refused', async () => {
    const { deck } = io({ nope: true });
    await expect(omarchyActions(deck).status('a')).rejects.toThrow('invalid Omarchy status');
  });

  test('act sends the action with the elevation header', async () => {
    const { deck, sent } = io(result());
    const got = await omarchyActions(deck).act('alpha', 'screensaver_on', 'tok');
    expect(sent).toEqual([
      {
        method: 'POST',
        path: '/api/stations/alpha/omarchy',
        body: { action: 'screensaver_on' },
        headers: { [ELEVATION_HEADER]: 'tok' },
      },
    ]);
    expect(ELEVATION_HEADER).toBe('x-fleetdeck-elevation');
    expect(got.outcome).toBe('done');
  });

  test('an invalid answer is refused', async () => {
    const { deck } = io({ outcome: 'maybe', status: status() });
    await expect(omarchyActions(deck).act('a', 'install', 't')).rejects.toThrow(
      'invalid Omarchy answer',
    );
  });
});

describe('stateOfReadError', () => {
  test('501 is unsupported, 404 hides the card, anything else is an error', () => {
    expect(stateOfReadError(new DeckError(501, 'x', 'unsupported', {}))).toEqual({
      kind: 'unsupported',
    });
    expect(stateOfReadError(new DeckError(404, 'x', null, {}))).toEqual({ kind: 'hidden' });
    expect(stateOfReadError(new DeckError(409, 'x', 'station_unreachable', {}))).toEqual({
      kind: 'error',
    });
    expect(stateOfReadError(new Error('boom'))).toEqual({ kind: 'error' });
  });
});

const noop = () => {};
const card = (
  state: CardState,
  extra: { busy?: boolean; failure?: { text: string; message: string | null } } = {},
) =>
  renderToStaticMarkup(
    <OmarchyView
      state={state}
      busy={extra.busy ?? false}
      failure={extra.failure ?? null}
      onAct={noop}
      onRetryRead={noop}
    />,
  );

describe('OmarchyView', () => {
  test('loading', () => {
    expect(card({ kind: 'loading' })).toContain('Loading the Omarchy state…');
  });

  test('501: the text of an unsupported build', () => {
    const html = card({ kind: 'unsupported' });
    expect(html).toContain('This station&#x27;s build cannot manage Omarchy from the board.');
    expect(html).not.toContain('<button');
  });

  test('404: no card', () => {
    expect(card({ kind: 'hidden' })).toBe('');
  });

  test('another error: the text and a Retry button', () => {
    const html = card({ kind: 'error' });
    expect(html).toContain('The Omarchy state could not be read.');
    expect(html).toContain('Retry');
  });

  test('ready: the lines and the buttons', () => {
    const html = card({ kind: 'ready', status: status() });
    expect(html).toContain('Omarchy 3.1.0');
    expect(html).toContain('Integration: installed');
    expect(html).toContain('>Uninstall</button>');
    expect(html).not.toContain('disabled');
    expect(html).not.toContain('Working…');
  });

  test('an action runs: Working… and every button off', () => {
    const html = card({ kind: 'ready', status: status() }, { busy: true });
    expect(html).toContain('Working…');
    expect(html.match(/<button/g)?.length).toBe(html.match(/<button[^>]*disabled/g)?.length);
  });

  test('a running action of the station: Working… once, no button', () => {
    const html = card({ kind: 'ready', status: status({ running: 'install' }) }, { busy: true });
    expect(html.match(/Working…/g)?.length).toBe(1);
    expect(html).not.toContain('<button');
  });

  test('a failed action: the text, and the message as plain text in a pre', () => {
    const html = card(
      { kind: 'ready', status: status() },
      { failure: { text: 'The command failed.', message: '<img src=x onerror=alert(1)>' } },
    );
    expect(html).toContain('The command failed.');
    expect(html).toContain('<pre>&lt;img src=x onerror=alert(1)&gt;</pre>');
    expect(html).not.toContain('<img');
  });
});

describe('SettingsTab with the card', () => {
  const settings = {
    view: async () => view(),
    elevation: () => null,
  } as unknown as SettingsActions;
  const tab = (props: { live: boolean; omarchy?: OmarchyActions; stationId?: string | null }) =>
    renderToStaticMarkup(
      <SettingsTab
        stationId={props.stationId === undefined ? 'st-alpha' : props.stationId}
        name="Alpha"
        actions={settings}
        live={props.live}
        initial={{ view: view() }}
        omarchy={props.omarchy}
      />,
    );
  const { actions } = fake({});

  test('a live station tab with the calls shows the card', () => {
    expect(tab({ live: true, omarchy: actions })).toContain('aria-label="Omarchy"');
  });

  test('no card when the station is not live, the calls are missing, or this is the deck tab', () => {
    expect(tab({ live: false, omarchy: actions })).not.toContain('aria-label="Omarchy"');
    expect(tab({ live: true })).not.toContain('aria-label="Omarchy"');
    expect(tab({ live: true, omarchy: actions, stationId: null })).not.toContain(
      'aria-label="Omarchy"',
    );
  });
});
