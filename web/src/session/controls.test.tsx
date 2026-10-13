// DL-140: the session controls (controller flags, header, approval dock, composer, slash menu).
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DeckSession, ModelChoice, OpenRequest } from '../gen';
import { claudeCaps, minimalCaps } from '../testing/capabilities';
import { card, station } from '../testing/fleet';
import { ApprovalDock } from './ApprovalDock';
import { sessionActions } from './commands';
import { Composer } from './Composer';
import { type Controller, sessionFlags } from './controller';
import { SessionHeader } from './SessionHeader';
import { ModelPicker } from './SessionSettings';
import { matchSlash } from './slash';

const key = { station_id: 'station-a', session_id: 's1' };
const drive = sessionActions(async () => ({}), key, ['read', 'drive', 'approve']);
const readOnly = sessionActions(async () => ({}), key, ['read']);

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

const ended = () => session({ state: 'exited', current_run: null }, 'ended');
const held = () =>
  session(
    {
      state: 'exited',
      current_run: null,
      terminal: { shell_id: 'sh-1', pid: 42, since: 1, reason: null },
    },
    'ended',
  );

describe('session flags (DL-140)', () => {
  test('a running live session can be driven, stopped and interrupted', () => {
    expect(sessionFlags(session(), 'live', drive)).toEqual({
      live: true,
      ended: false,
      held: false,
      working: true,
      drive: true,
      approve: true,
      canResume: false,
      canStop: true,
      canInterrupt: true,
    });
  });

  test('an ended session with the resume capability can be resumed but not stopped', () => {
    const flags = sessionFlags(ended(), 'live', drive);
    expect(flags).toMatchObject({ ended: true, canResume: true, canStop: false, drive: false });
    expect(sessionFlags(ended(), 'live', readOnly).canResume).toBe(false);
    const noResume = session({ state: 'exited', capabilities: minimalCaps() }, 'ended');
    expect(sessionFlags(noResume, 'live', drive).canResume).toBe(false);
  });

  test('a session held by a terminal cannot be driven, resumed or interrupted', () => {
    const flags = sessionFlags(held(), 'live', drive);
    expect(flags).toMatchObject({
      held: true,
      drive: false,
      canResume: false,
      canInterrupt: false,
    });
    const running = session({
      terminal: { shell_id: 'sh-1', pid: null, since: 1, reason: null },
    });
    expect(sessionFlags(running, 'live', drive)).toMatchObject({ held: true, drive: false });
  });

  test('a stale board or an unreachable station turns every control off', () => {
    for (const flags of [
      sessionFlags(session(), 'stale', drive),
      sessionFlags(session({}, 'unreachable'), 'live', drive),
    ]) {
      expect(flags).toMatchObject({
        live: false,
        drive: false,
        approve: false,
        canStop: false,
        canResume: false,
        canInterrupt: false,
      });
    }
  });

  test('a working session without the interrupt capability has no interrupt', () => {
    const caps = { ...claudeCaps(), interrupt: false };
    const flags = sessionFlags(session({ capabilities: caps }), 'live', drive);
    expect(flags).toMatchObject({ working: true, drive: true, canInterrupt: false });
  });
});

describe('slash menu matching (DL-140)', () => {
  const commands = ['compact', 'init', 'review', 'clear', 'cost', 'pr-comments'];

  test('text without a leading slash does not match', () => {
    expect(matchSlash('hello', commands)).toBeNull();
    expect(matchSlash('', commands)).toBeNull();
    expect(matchSlash(' /c', commands)).toBeNull();
  });

  test('commands that start with the query come before commands that contain it', () => {
    expect(matchSlash('/c', commands)).toEqual({
      query: 'c',
      options: ['compact', 'clear', 'cost', 'pr-comments'],
    });
    expect(matchSlash('/CO', commands)?.options).toEqual(['compact', 'cost', 'pr-comments']);
    expect(matchSlash('/', commands)?.options).toEqual(commands);
  });

  test('at most 8 options and no duplicates', () => {
    const many = Array.from({ length: 12 }, (_, n) => `cmd${n}`);
    expect(matchSlash('/cmd', many)?.options).toHaveLength(8);
    expect(matchSlash('/a', ['a', 'a', 'ab'])?.options).toEqual(['a', 'ab']);
  });

  test('a space after the command closes the match', () => {
    expect(matchSlash('/compact ', commands)).toBeNull();
    expect(matchSlash('/compact now', commands)).toBeNull();
  });
});

const idle: Controller = {
  flags: sessionFlags(session(), 'live', drive),
  busy: null,
  outcome: null,
  act: () => {},
  dismiss: () => {},
};

function header(
  s: DeckSession,
  extra: {
    terminal?: { available: boolean } | null;
    actions?: typeof drive | null;
    onNewShell?: () => void;
  } = {},
) {
  const actions = extra.actions === undefined ? drive : extra.actions;
  const terminal = extra.terminal ? { ...extra.terminal, onOpen() {}, onReturn() {} } : null;
  return renderToStaticMarkup(
    <SessionHeader
      session={s}
      stations={[station()]}
      controller={{ ...idle, flags: sessionFlags(s, 'live', actions) }}
      actions={actions}
      terminal={terminal}
      {...(extra.onNewShell ? { onNewShell: extra.onNewShell } : {})}
    />,
  );
}

/** The markup of the "Terminal" menu of the header, or '' without one. */
function terminalMenu(html: string): string {
  const match = /<details class="fd-menu"><summary aria-label="Terminal">[\s\S]*?<\/details>/.exec(
    html,
  );
  return match?.[0] ?? '';
}

describe('session header (DL-140)', () => {
  test('a rate limit of 80% or more shows a chip; 100% is a hazard (R4.2)', () => {
    const limited = (used: number) =>
      session({ rate_limits: [{ scope: 'five_hour', used_percent: used, resets_at: null }] });
    expect(header(limited(85))).toContain('data-tone="act">Limit 85% · five_hour</span>');
    expect(header(limited(100))).toContain('data-tone="hazard">Limit reached · five_hour</span>');
    expect(header(limited(79))).not.toContain('Limit ');
    expect(header(session())).not.toContain('Limit ');
  });

  test('a running session offers Interrupt and Stop', () => {
    const html = header(session());
    expect(html).toContain('aria-label="Session header"');
    expect(html).toContain('Interrupt');
    // Stop and Force stop are items of the session menu
    expect(html).toContain('aria-label="Session menu"');
    expect(html).toMatch(/<button[^>]*>(<svg[\s\S]*?<\/svg>)? Stop<\/button>/);
    expect(html).toContain('Force stop…');
    expect(html).toContain('Fix login');
    expect(html).toContain('Alpha');
    expect(html).not.toContain('Resume');
  });

  test('an ended session offers Resume and no Stop', () => {
    const html = header(ended());
    expect(html).toContain('Resume');
    expect(html).not.toMatch(/ Stop<\/button>/);
    expect(html).not.toContain('Force stop');
    expect(html).not.toContain('Interrupt');
  });

  test('a held session shows In terminal and Return to board control, no Resume', () => {
    const html = header(held(), { terminal: { available: true } });
    expect(html).toContain('In terminal');
    expect(html).toContain('Return to board control');
    expect(html).not.toContain('Resume');
    expect(html).not.toContain('Open in terminal');
  });

  test('an available terminal offers Open in terminal', () => {
    expect(header(session(), { terminal: { available: true } })).toContain('Open in terminal');
    expect(header(ended(), { terminal: { available: true } })).toContain('Open in terminal');
    expect(header(session(), { terminal: { available: false } })).not.toContain('Open in terminal');
    expect(header(session(), { terminal: null })).not.toContain('Open in terminal');
  });

  test('the Terminal menu offers Open shell in workspace, and Open in terminal when it is possible', () => {
    const onNewShell = () => {};
    const shellOnly = terminalMenu(header(session(), { onNewShell }));
    expect(shellOnly).toContain('Open shell in workspace');
    expect(shellOnly).not.toContain('Open in terminal');
    const both = terminalMenu(header(session(), { onNewShell, terminal: { available: true } }));
    expect(both).toContain('Open shell in workspace');
    expect(both).toContain('fd-menu-sep');
    expect(both).toContain('Open in terminal…');
    expect(both.indexOf('Open shell in workspace')).toBeLessThan(both.indexOf('Open in terminal…'));
    // Without a new-shell action, only the handover shows; without both, there is no menu.
    const handover = terminalMenu(header(session(), { terminal: { available: true } }));
    expect(handover).not.toContain('Open shell in workspace');
    expect(handover).toContain('Open in terminal…');
    expect(terminalMenu(header(session()))).toBe('');
    // A held session keeps the shell item and no handover item.
    const heldMenu = terminalMenu(header(held(), { onNewShell, terminal: { available: true } }));
    expect(heldMenu).toContain('Open shell in workspace');
    expect(heldMenu).not.toContain('Open in terminal');
  });

  test('a station with shells off gives no Open in terminal and says why in the menu', () => {
    const html = header(session(), { terminal: { available: false } });
    expect(html).not.toContain('aria-label="Open in terminal"');
    expect(html).toContain(
      'Shells are off on this station. Set allow_shell = true in station.toml to open a terminal.',
    );
    // an available terminal does not show the text
    expect(header(session(), { terminal: { available: true } })).not.toContain('Shells are off');
    // a board without the admin scope has no terminal: no control and no text
    expect(header(session(), { terminal: null })).not.toContain('Shells are off');
  });

  test('the mode badge shows the mode with its tone, and is absent without a mode', () => {
    const badge = (mode: string) =>
      new RegExp(
        `<span class="fd-badge" data-tone="([a-z]+)" title="Permission mode">${mode}</span>`,
      );
    const tone = (mode: string) => badge(mode).exec(header(session({ mode })))?.[1];
    expect(tone('plan')).toBe('plan');
    expect(tone('dontAsk')).toBe('hazard');
    expect(tone('default')).toBe('muted');
    expect(header(session({ mode: null }))).not.toContain('Permission mode');
  });

  test('the title falls back to the callsign, the model to "default model"', () => {
    const html = header(session({ title: null, callsign: 'otter', model: null }));
    expect(html).toContain('>otter</h2>');
    expect(html).toContain('default model');
  });

  test('a long cwd is shortened in the middle and kept whole in the title', () => {
    const cwd = `/home/someone/${'deep/'.repeat(20)}repo`;
    const html = header(session({ cwd, branch: 'main' }));
    expect(html).toContain(`title="${cwd}"`);
    expect(html).toContain('main');
    expect(html).toMatch(/…/);
    expect(html).not.toContain(`>${cwd}<`);
  });

  test('a stale board says the controls are off', () => {
    const html = renderToStaticMarkup(
      <SessionHeader
        session={session()}
        stations={[station()]}
        controller={{ ...idle, flags: sessionFlags(session(), 'stale', drive) }}
        actions={drive}
        terminal={null}
      />,
    );
    expect(html).toContain('Controls are off until the board and the station are live.');
    expect(html).not.toContain('Interrupt');
  });

  test('the outcome has a role and a Dismiss icon button', () => {
    const html = renderToStaticMarkup(
      <SessionHeader
        session={session()}
        stations={[station()]}
        controller={{ ...idle, outcome: { ok: false, text: 'Refused: no' } }}
        actions={drive}
        terminal={null}
      />,
    );
    expect(html).toContain('role="alert"');
    expect(html).toContain('Refused: no');
    expect(html).toContain('aria-label="Dismiss"');
    expect(html).not.toContain('>Dismiss<');
  });

  test('the settings menu holds mode, model and auto-continue', () => {
    const html = header(session());
    expect(html).toContain('aria-label="Session menu"');
    expect(html).not.toContain('aria-label="Session settings"');
    expect(html).toContain('Mode (most restrictive first)');
    expect(html).toContain('Set model');
    expect(html).toContain('Continue after a station restart');
  });

  test('the details block lists the facts of the session', () => {
    const html = header(
      session({ callsign: 'falcon', worktree: '/wt/fix', session_seq: 42, updated_at: Date.now() }),
    );
    expect(html).toContain('class="fd-sh-details"');
    expect(html).toContain('<dt>Session</dt><dd><span>s1</span>');
    expect(html).toContain('aria-label="Copy the session id"');
    expect(html).toContain('<dt>Callsign</dt><dd>falcon</dd>');
    expect(html).toContain('<dt>Worktree</dt><dd>/wt/fix</dd>');
    expect(html).toContain('<dt>Updated</dt><dd>now</dd>');
    expect(html).toContain('<dt>Events</dt><dd>42</dd>');
    const bare = header(session());
    expect(bare).not.toContain('<dt>Callsign</dt>');
    expect(bare).not.toContain('<dt>Worktree</dt>');
  });

  test('the menu offers a status update, Star and (when ended) Archive', () => {
    const html = header(session());
    expect(html).toMatch(/<button[^>]*>(<svg[\s\S]*?<\/svg>)? Ask for a status update<\/button>/);
    expect(html).toMatch(/<button[^>]*>(<svg[\s\S]*?<\/svg>)? Star<\/button>/);
    expect(html).not.toContain('Archive');
    expect(html).not.toContain('fd-sh-star');
    const done = header(ended());
    expect(done).not.toContain('Ask for a status update');
    expect(done).toMatch(/<button[^>]*>(<svg[\s\S]*?<\/svg>)? Archive<\/button>/);
    expect(header(session({}, 'lost'))).toContain(' Archive</button>');
    expect(header(session(), { actions: readOnly })).not.toContain('Ask for a status update');
  });

  test('the last error shows when the session ended or has no run', () => {
    const error = { code: 'usage_limit', message: 'The limit is reached', retry_at: null };
    const strip = 'data-tone="hazard" role="status"';
    expect(header(ended())).not.toContain(strip);
    const shown = header(
      session({ state: 'exited', current_run: null, last_error: error }, 'ended'),
    );
    expect(shown).toContain(strip);
    expect(shown).toContain('usage_limit: The limit is reached');
    expect(shown).not.toContain('retry in');
    expect(header(session({ current_run: null, last_error: error }, 'idle'))).toContain(strip);
    // a running session with a run hides an old error
    expect(header(session({ last_error: error }))).not.toContain(strip);
  });

  test('the last error tells when a retry is possible', () => {
    const at = (ms: number) => ({ code: 'rate', message: 'slow down', retry_at: Date.now() + ms });
    const text = (ms: number) => header(session({ current_run: null, last_error: at(ms) }, 'idle'));
    expect(text(5 * 60_000 + 30_000)).toContain('slow down · retry in 6 min');
    expect(text(40_000)).toMatch(/slow down · retry in \d+ s/);
    expect(text(-1000)).not.toContain('retry in');
  });
});

function request(id: string): OpenRequest {
  return {
    request_id: id,
    node_id: null,
    request_kind: 'tool',
    title: `Run ${id}`,
    details: 'rm -rf dist',
    input: null,
    options: [
      { option_id: 'allow', label: 'Allow', kind: 'allow_once' },
      { option_id: 'deny', label: 'Deny', kind: 'reject_once' },
    ],
    response: 'rpc_reply',
    requested_at: 1,
  } as unknown as OpenRequest;
}

describe('approval dock (DL-140)', () => {
  const dock = (requests: OpenRequest[], answerable = true, enabled = true) =>
    renderToStaticMarkup(
      <ApprovalDock
        requests={requests}
        answerable={answerable}
        enabled={enabled}
        busy={false}
        onAnswer={() => {}}
      />,
    );

  test('no request renders nothing', () => {
    expect(dock([])).toBe('');
  });

  test('two requests show the count and one primary option each', () => {
    const html = dock([request('r1'), request('r2')]);
    expect(html).toContain('Needs you · 2');
    expect(html).toContain('aria-label="Needs you"');
    expect(html.match(/class="fd-primary"/g)).toHaveLength(2);
    expect(html).toContain('Deny');
  });

  test('without the approvals capability or the scope it says why', () => {
    expect(dock([request('r1')], false)).toContain('This provider has no approval controls.');
    expect(dock([request('r1')], true, false)).toContain('Answering needs the approve scope.');
  });
});

describe('composer (DL-140)', () => {
  const composer = (extra: Partial<Parameters<typeof Composer>[0]> = {}) =>
    renderToStaticMarkup(
      <Composer enabled working={false} steer={false} nativeQueue onSend={() => {}} {...extra} />,
    );

  test('the placeholder mentions commands only with a command list', () => {
    expect(composer({ slashCommands: ['init'] })).toContain(
      'placeholder="Message the agent… (/ for commands)"',
    );
    expect(composer({ slashCommands: [] })).toContain('placeholder="Message the agent…"');
    expect(composer({ slashCommands: null })).toContain('placeholder="Message the agent…"');
    expect(composer()).toContain('placeholder="Message the agent…"');
  });

  test('a disabled composer shows its reason instead of the follow-up note', () => {
    const reason = 'A terminal holds this session. Return it to board control to send prompts.';
    const html = composer({ enabled: false, working: true, disabledReason: reason });
    expect(html).toContain(reason);
    expect(html).not.toContain('The station holds a follow-up');
    expect(composer({ enabled: true, disabledReason: reason })).not.toContain(reason);
  });

  test('the buttons keep their texts and the textarea keeps its label', () => {
    expect(composer()).toMatch(/<\/svg> Send<\/button>/);
    expect(composer()).toContain('>Message</label>');
    expect(composer()).toContain('Add image');
    const working = composer({ working: true, steer: true, nativeQueue: false });
    expect(working).toContain('Steer now');
    expect(working).toContain('Queue follow-up');
    expect(working).toContain('The station holds a follow-up until the turn ends.');
  });
});

describe('stop and resume commands (DL-140)', () => {
  test('stop posts session.stop with how, resume posts session.resume', async () => {
    const posts: { path: string; body: { command: unknown } }[] = [];
    const actions = sessionActions(
      async (path, body) => {
        posts.push({ path, body: body as { command: unknown } });
        return { state: 'done', receipt: { state: 'accepted' } };
      },
      key,
      ['drive'],
      () => 'cmd-1',
    );
    expect(await actions.stop('graceful')).toEqual({ ok: true, text: 'Done.' });
    await actions.stop('kill');
    await actions.resume();
    expect(posts.map((p) => p.path)).toEqual([
      '/api/commands/session.stop',
      '/api/commands/session.stop',
      '/api/commands/session.resume',
    ]);
    expect(posts.map((p) => p.body.command)).toEqual([
      { op: 'stop', session_id: 's1', how: 'graceful' },
      { op: 'stop', session_id: 's1', how: 'kill' },
      { op: 'resume', session_id: 's1' },
    ]);
  });
});

describe('model picker (DL-151)', () => {
  const models: ModelChoice[] = [
    { value: 'default', label: 'Default', description: 'The recommended model' },
    { value: 'opus', label: 'Opus', description: null },
  ];
  const picker = (current: string | null, list: readonly ModelChoice[] | null, enabled = true) =>
    renderToStaticMarkup(
      <ModelPicker current={current} models={list} enabled={enabled} onSet={() => {}} />,
    );

  test('a list renders a select with the models and Other…', () => {
    const html = picker('opus', models);
    expect(html).toContain('<select');
    expect(html).toContain('aria-label="Model"');
    expect(html).toContain('Model (now opus)');
    expect(html).toContain('>Default</option>');
    expect(html).toContain('title="The recommended model"');
    expect(html).toContain('>Other…</option>');
    // no text form until "Other…" is chosen
    expect(html).not.toContain('Set model');
  });

  test('the option that equals the current model is selected', () => {
    const html = picker('opus', models);
    expect(html).toMatch(/<option value="opus" selected/);
    expect(html).not.toMatch(/<option value="" disabled="" selected/);
  });

  test('a current id that matches no value selects the placeholder', () => {
    const html = picker('claude-opus-5-5[1m]', models);
    expect(html).toMatch(/<option value="" disabled="" selected="">Choose a model<\/option>/);
    expect(html).toContain('Model (now claude-opus-5-5[1m])');
  });

  test('without models the free-text form stays', () => {
    for (const list of [null, []]) {
      const html = picker(null, list);
      expect(html).not.toContain('<select');
      expect(html).toContain('Model (now unknown)');
      expect(html).toContain('placeholder="Model id or alias"');
      expect(html).toContain('Set model');
    }
  });

  test('a disabled picker disables the select', () => {
    expect(picker('opus', models, false)).toMatch(/<select[^>]*disabled/);
    expect(picker('opus', null, false)).toMatch(/<input[^>]*disabled/);
  });
});
