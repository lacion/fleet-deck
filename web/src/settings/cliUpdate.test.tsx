// B28 (design `cli-update.md` §3.12): the "Update now" card of Settings: its texts, its calls
// and its poll.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { CliUpdateReport, ProviderUpdateState } from '../gen';
import { type DialogState, type Note, CliUpdateView, UpdateDialog } from './CliUpdateCard';
import {
  type CliUpdateActions,
  type CliUpdateRunBody,
  askUpdate,
  busyView,
  checkLine,
  dialogText,
  pollUntilIdle,
  reasonText,
  rowTitle,
  runCheck,
  runUpdate,
} from './cliUpdate';
import { answer, attempt, NOW, report, state } from './cliUpdateFixtures';
import { DeckError } from './http';
import { groupFields } from './model';
import { meta } from './fixtures';

const noop = () => undefined;

type Call = { body: CliUpdateRunBody; elevation: string | null };

/** Fake calls: `run` answers from `answers` in order and records its calls. */
function fake(answers: unknown[], reports: CliUpdateReport[] = []) {
  const calls: Call[] = [];
  let reads = 0;
  const actions: CliUpdateActions = {
    status: async () => {
      const next = reports[Math.min(reads, reports.length - 1)];
      reads += 1;
      if (next === undefined) throw new Error('no report');
      return next;
    },
    run: async (_station, body, elevation) => {
      calls.push({ body, elevation });
      const next = answers.shift();
      if (next instanceof Error) throw next;
      return next as ReturnType<typeof answer>;
    },
  };
  return { actions, calls, reads: () => reads };
}

const view = (
  r: CliUpdateReport,
  extra: {
    notes?: Record<string, Note>;
    busy?: boolean;
    dialog?: DialogState | null;
    pins?: { claude: string | null; codex: string | null };
  } = {},
) =>
  renderToStaticMarkup(
    <CliUpdateView
      station="Lighting"
      state={{ kind: 'ready', report: r }}
      pins={extra.pins ?? { claude: null, codex: null }}
      now={NOW}
      notes={extra.notes ?? {}}
      busy={extra.busy ?? false}
      dialog={extra.dialog ?? null}
      onCheck={noop}
      onUpdate={noop}
      onRetry={noop}
      onForce={noop}
      onConfirm={noop}
      onClose={noop}
    />,
  );

describe('the row of a provider', () => {
  test('the title names the version, the method and the channel', () => {
    expect(rowTitle(state(), null)).toBe('Claude 2.1.309 · native installer · stable channel');
    expect(rowTitle(state({ provider: 'codex', method: 'bun', channel: 'latest' }), null)).toBe(
      'Codex 2.1.309 · Bun · latest channel',
    );
    expect(rowTitle(state({ channel: 'pin' }), '2.1.305')).toBe(
      'Claude 2.1.309 · native installer · pinned to 2.1.305',
    );
    expect(rowTitle(state({ installed: null }), null)).toStartWith('Claude · ');
  });
  test('the card shows the title, the last attempt and both buttons', () => {
    const html = view(report({ providers: [state({ last: attempt() })] }));
    expect(html).toContain('<h2>Update now</h2>');
    expect(html).toContain('Claude 2.1.309 · native installer · stable channel');
    expect(html).toContain('Claude: updated 2.1.308 → 2.1.310 · 3 h ago');
    expect(html).toContain('Check');
    expect(html).toContain('Update now…');
  });
  test('a provider that is not updatable shows the blocked text and no Update now…', () => {
    const html = view(
      report({
        providers: [
          state({
            updatable: false,
            blocked: 'gpgv_missing',
            last: attempt({ outcome: 'up_to_date' }),
          }),
        ],
      }),
    );
    expect(html).toContain('Not updated: install GnuPG (gpgv) to verify Claude updates.');
    expect(html).not.toContain('Update now…');
    expect(html).toContain('Check');
  });
  test('a provider that only reports shows the hand line and no Update now…', () => {
    const html = view(
      report({
        providers: [state({ updatable: false, method: 'homebrew', blocked: 'report_only_method' })],
      }),
    );
    expect(html).toContain('Claude: update by hand (Homebrew)');
    expect(html).not.toContain('Update now…');
  });
  test('a running update shows the running text and turns the buttons off', () => {
    const html = view(
      report({
        running: 'a1',
        providers: [state({ last: attempt({ outcome: 'running', target_version: '2.1.310' }) })],
      }),
    );
    expect(html).toContain('Updating Claude to 2.1.310…');
    expect(html.match(/disabled=""/g)?.length).toBe(2);
  });
  test('a note shows under the row', () => {
    const html = view(report(), {
      notes: { claude: { text: '2.1.310 is available.', tone: 'info' } },
    });
    expect(html).toContain('2.1.310 is available.');
    const failed = view(report(), {
      notes: { claude: { text: 'Not updated: x.', tone: 'error' } },
    });
    expect(failed).toContain('role="alert"');
  });
  test('the pins come from the settings', () => {
    const html = view(report({ providers: [state({ channel: 'pin' })] }), {
      pins: { claude: '2.1.305', codex: null },
    });
    expect(html).toContain('pinned to 2.1.305');
  });
});

describe('Check', () => {
  test('it sends check_only and no elevation', async () => {
    const f = fake([answer()]);
    await runCheck(f.actions, 'Lighting', state());
    expect(f.calls).toEqual([
      { body: { provider: 'claude', check_only: true, force: false }, elevation: null },
    ]);
  });
  test('the result lines', () => {
    expect(checkLine(answer({ state: state({ available: '2.1.310' }) }), 'claude', false)).toBe(
      '2.1.310 is available.',
    );
    expect(checkLine(answer(), 'claude', false)).toBe('Up to date.');
    expect(checkLine(answer({ refused: 'too_new' }), 'claude', false)).toBe(
      'Not available: the new version is still in its wait time.',
    );
  });
  test('a quarantined provider shows the pass and the still-blocked texts', () => {
    expect(checkLine(answer(), 'claude', true)).toBe(
      'Claude passed the checks. Sessions can start again.',
    );
    expect(checkLine(answer({ refused: 'handshake_failed' }), 'claude', true)).toBe(
      'Still blocked: Fleet Deck could not drive the new version.',
    );
    expect(
      checkLine(answer({ state: state({ quarantined: 'rollback_failed' }) }), 'codex', true),
    ).toBe('Still blocked: see the station.');
  });
  test('a failed call gives a text, and a 501 gives the unsupported text', async () => {
    const f = fake([new Error('boom'), new DeckError(501, 'x', 'cli_update_unsupported', {})]);
    expect(await runCheck(f.actions, 'Lighting', state())).toEqual({
      kind: 'failed',
      text: 'boom',
    });
    expect(await runCheck(f.actions, 'Lighting', state())).toEqual({
      kind: 'failed',
      text: 'This station cannot update its providers yet. Update the station first.',
    });
  });
});

describe('Update now…', () => {
  test('it asks the elevation with the update label', async () => {
    let labels: readonly string[] | undefined;
    const token = await askUpdate(async (l) => {
      labels = l;
      return 'tok';
    });
    expect(token).toBe('tok');
    expect(labels).toEqual(['update']);
    expect(await askUpdate(async () => null)).toBeNull();
  });
  test('the Claude dialog text', () => {
    expect(dialogText(state({ available: '2.1.310' }))).toBe(
      'The station downloads Claude 2.1.310 from Anthropic (stable channel), checks it against the signed release and checks that Fleet Deck can drive it before it switches. If a check fails after the switch, it goes back to 2.1.309 from a local copy. New sessions wait a few seconds while it switches.',
    );
    expect(dialogText(state())).toContain('Claude the newest version from Anthropic');
  });
  test('the Codex dialog text, with npm or Bun', () => {
    const codex = state({
      provider: 'codex',
      method: 'npm',
      channel: 'latest',
      installed: '0.163.0',
    });
    expect(dialogText({ ...codex, available: '0.164.0' })).toBe(
      'The station downloads Codex 0.164.0 from the npm registry with npm into a separate folder, checks every package against the registry signature and checks that Fleet Deck can drive it before it switches. If a check fails after the switch, it puts 0.163.0 back from a local copy. New sessions wait a few seconds while it switches.',
    );
    expect(dialogText({ ...codex, method: 'bun' })).toContain('with Bun into a separate folder');
  });
  test('the dialog has its title and the buttons Update and Cancel', () => {
    const open: DialogState = {
      state: state(),
      elevation: 't',
      busy: false,
      sessions: null,
      force: false,
    };
    const html = renderToStaticMarkup(
      <UpdateDialog
        station="Lighting"
        dialog={open}
        onForce={noop}
        onUpdate={noop}
        onClose={noop}
      />,
    );
    expect(html).toContain('Update Claude on Lighting?');
    expect(html).toContain('>Update<');
    expect(html).toContain('>Cancel<');
    expect(html).not.toContain('checkbox');
    const codex = renderToStaticMarkup(
      <UpdateDialog
        station="Lighting"
        dialog={{ ...open, state: state({ provider: 'codex' }) }}
        onForce={noop}
        onUpdate={noop}
        onClose={noop}
      />,
    );
    expect(codex).toContain('Update Codex on Lighting?');
  });
  test('the update sends the elevation, and force only when it is true', async () => {
    const f = fake([answer({ attempt_id: 'a1' }), answer({ attempt_id: 'a2' })]);
    await runUpdate(f.actions, 'Lighting', 'claude', 'tok', false);
    await runUpdate(f.actions, 'Lighting', 'claude', 'tok', true);
    expect(f.calls[0]).toEqual({
      body: { provider: 'claude', check_only: false, force: false },
      elevation: 'tok',
    });
    expect(f.calls[1]?.body.force).toBe(true);
  });
  test('a started update gives the answer', async () => {
    const f = fake([answer({ attempt_id: 'a1' })]);
    const out = await runUpdate(f.actions, 'Lighting', 'claude', 'tok', false);
    expect(out.kind).toBe('started');
  });
});

describe('the busy and the refused answers', () => {
  const busy = (extra: Partial<ProviderUpdateState>, n: number) =>
    answer({
      refused: 'sessions_running',
      busy: Array.from({ length: n }, (_, i) => `s${i}`) as never,
      state: state(extra),
    });
  test('native Claude names the sessions and offers the check box', () => {
    expect(busyView(busy({}, 2))).toEqual({
      text: '2 Claude sessions run on this station. They keep their version; new sessions use the new one.',
      canForce: true,
    });
    const open: DialogState = {
      state: state(),
      elevation: 't',
      busy: false,
      sessions: busyView(busy({}, 2)),
      force: false,
    };
    const html = renderToStaticMarkup(
      <UpdateDialog
        station="Lighting"
        dialog={open}
        onForce={noop}
        onUpdate={noop}
        onClose={noop}
      />,
    );
    expect(html).toContain('Update while they run');
    expect(html).toContain('>Update<');
  });
  test('npm and Bun say to stop the sessions, with no check box and no Update', () => {
    const codex = busy({ provider: 'codex', method: 'npm' }, 1);
    expect(busyView(codex)).toEqual({
      text: '1 Codex sessions run on this station. An update replaces the files in place: stop these sessions first.',
      canForce: false,
    });
    const html = renderToStaticMarkup(
      <UpdateDialog
        station="Lighting"
        dialog={{
          state: codex.state,
          elevation: 't',
          busy: false,
          sessions: busyView(codex),
          force: false,
        }}
        onForce={noop}
        onUpdate={noop}
        onClose={noop}
      />,
    );
    expect(html).not.toContain('Update while they run');
    expect(html).not.toContain('>Update<');
  });
  test('a busy answer comes back as busy, not as a refusal', async () => {
    const f = fake([busy({}, 3)]);
    const out = await runUpdate(f.actions, 'Lighting', 'claude', 'tok', false);
    expect(out.kind).toBe('busy');
  });
  test('recovery_pending names the provider and the restart', async () => {
    const f = fake([answer({ refused: 'recovery_pending', state: state({ provider: 'codex' }) })]);
    expect(await runUpdate(f.actions, 'Lighting', 'codex', 'tok', false)).toEqual({
      kind: 'refused',
      text: 'An interrupted update of Codex waits for the station restart. Restart the station first.',
    });
  });
  test('another refusal says Not updated, and an unknown code says see the station', async () => {
    const f = fake([answer({ refused: 'disk_full' }), answer({ refused: 'brand_new_code' })]);
    expect(await runUpdate(f.actions, 'Lighting', 'claude', 't', false)).toEqual({
      kind: 'refused',
      text: 'Not updated: less than 1 GB free.',
    });
    expect(await runUpdate(f.actions, 'Lighting', 'claude', 't', false)).toEqual({
      kind: 'refused',
      text: 'Not updated: see the station.',
    });
  });
  test('the 501 and an ended elevation have their texts', async () => {
    const f = fake([
      new DeckError(501, 'x', 'cli_update_unsupported', {}),
      new DeckError(400, 'x', 'elevation_required', { keys: ['update'] }),
    ]);
    const a = await runUpdate(f.actions, 'Lighting', 'claude', 't', false);
    expect(a).toEqual({
      kind: 'failed',
      text: 'This station cannot update its providers yet. Update the station first.',
    });
    const b = await runUpdate(f.actions, 'Lighting', 'claude', 't', false);
    expect(b).toEqual({
      kind: 'failed',
      text: 'Your confirmation ended. Press Update now… again.',
    });
  });
});

describe('the reason texts', () => {
  test('known codes have their text and an unknown code has the fallback', () => {
    expect(reasonText('quarantined')).toBe('starts are blocked after a failed update');
    expect(reasonText('process_left')).toBe('an update process did not stop');
    expect(reasonText('handshake_failed')).toBe('Fleet Deck could not drive the new version');
    expect(reasonText('toString')).toBe('see the station');
    expect(reasonText(null)).toBe('see the station');
  });
});

describe('the poll', () => {
  test('it reads every 3 s and stops when running is null', async () => {
    const f = fake(
      [],
      [report({ running: 'a1' }), report({ running: 'a1' }), report({ running: null })],
    );
    const waits: number[] = [];
    const seen: (string | null)[] = [];
    const done = await pollUntilIdle(f.actions, 'Lighting', {
      sleep: async (ms) => {
        waits.push(ms);
      },
      stopped: () => false,
      onReport: (r) => seen.push(r.running),
    });
    expect(done).toBe(true);
    expect(waits).toEqual([3000, 3000, 3000]);
    expect(seen).toEqual(['a1', 'a1', null]);
    expect(f.reads()).toBe(3);
  });
  test('it stops without a read when the card goes away', async () => {
    const f = fake([], [report({ running: 'a1' })]);
    const done = await pollUntilIdle(f.actions, 'Lighting', {
      sleep: async () => undefined,
      stopped: () => true,
      onReport: noop,
    });
    expect(done).toBe(false);
    expect(f.reads()).toBe(0);
  });
  test('a failed read ends the poll', async () => {
    const f = fake([], []);
    const done = await pollUntilIdle(f.actions, 'Lighting', {
      sleep: async () => undefined,
      stopped: () => false,
      onReport: noop,
    });
    expect(done).toBe(false);
  });
});

describe('the settings section', () => {
  test('Provider updates follows Providers and holds the eight keys', () => {
    const keys = [
      'provider_instance',
      'provider_updates.enabled',
      'provider_updates.window_start',
      'provider_updates.window_hours',
      'provider_updates.timezone',
      'provider_updates.claude_channel',
      'provider_updates.claude_pin',
      'provider_updates.codex_pin',
      'provider_updates.min_age_hours',
    ];
    const groups = groupFields(keys.map((k) => meta(k, k, 'text')));
    expect(groups.map((g) => g.title)).toEqual(['Providers', 'Provider updates']);
    expect(groups[1]?.fields.map((f) => f.key)).toEqual(keys.slice(1));
  });
});
