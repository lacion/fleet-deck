// DL-140: the session controls (controller flags, header, composer, slash menu); the approval
// dock is in approvalDock.test.tsx.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DeckSession, ModelChoice } from '../gen';
import { claudeCaps, minimalCaps } from '../testing/capabilities';
import { card, station } from '../testing/fleet';
import { matchAt } from './at';
import { sessionActions } from './commands';
import { Composer, ComposerMenu, searchFailure } from './Composer';
import { type Controller, sessionFlags } from './controller';
import { SessionHeader } from './SessionHeader';
import { ModelPicker } from './SessionSettings';
import { matchSlash } from './slash';
import { matchHash } from '../review/hashMenu';
import { submitKeyHint } from '../spawn/keyHint';

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
  const names = (text: string, list: readonly string[], skills: readonly string[] = []) =>
    matchSlash(text, list, skills)?.options.map((o) => o.name);
  const plain = (list: string[]) => list.map((name) => ({ name, skill: false }));

  test('text without a leading slash does not match', () => {
    expect(matchSlash('hello', commands)).toBeNull();
    expect(matchSlash('', commands)).toBeNull();
    expect(matchSlash(' /c', commands)).toBeNull();
  });

  test('commands that start with the query come before commands that contain it', () => {
    expect(matchSlash('/c', commands)).toEqual({
      query: 'c',
      options: plain(['compact', 'clear', 'cost', 'pr-comments']),
    });
    expect(names('/CO', commands)).toEqual(['compact', 'cost', 'pr-comments']);
    expect(names('/', commands)).toEqual(commands);
  });

  test('at most 8 options and no duplicates', () => {
    const many = Array.from({ length: 12 }, (_, n) => `cmd${n}`);
    expect(matchSlash('/cmd', many)?.options).toHaveLength(8);
    expect(names('/a', ['a', 'a', 'ab'])).toEqual(['a', 'ab']);
  });

  test('a command that is also a skill carries the skill flag (DL-174)', () => {
    expect(matchSlash('/', ['compact', 'deploy', 'review'], ['deploy', 'review'])?.options).toEqual(
      [
        { name: 'compact', skill: false },
        { name: 'deploy', skill: true },
        { name: 'review', skill: true },
      ],
    );
    expect(matchSlash('/dep', ['compact', 'deploy'], ['compact'])?.options).toEqual([
      { name: 'deploy', skill: false },
    ]);
  });

  test('a space after the command closes the match', () => {
    expect(matchSlash('/compact ', commands)).toBeNull();
    expect(matchSlash('/compact now', commands)).toBeNull();
  });
});

describe('at file menu matching (DL-174)', () => {
  test('a bare at and an at word with one character match', () => {
    expect(matchAt('@', 1)).toEqual({ query: '', start: 0 });
    expect(matchAt('see @', 5)).toEqual({ query: '', start: 4 });
    expect(matchAt('@a', 2)).toEqual({ query: 'a', start: 0 });
  });

  test('an at word with one character or more after it matches', () => {
    expect(matchAt('@a', 2)).toEqual({ query: 'a', start: 0 });
    expect(matchAt('see @src/ma', 11)).toEqual({ query: 'src/ma', start: 4 });
    expect(matchAt('@src/main.rs and', 12)).toEqual({ query: 'src/main.rs', start: 0 });
  });

  test('the word ends at the cursor', () => {
    expect(matchAt('@src/main.rs', 4)).toEqual({ query: 'src', start: 0 });
    expect(matchAt('fix @a.ts now', 9)).toEqual({ query: 'a.ts', start: 4 });
  });

  test('no match without a query, in the middle of a word, or outside the word', () => {
    expect(matchAt('', 0)).toBeNull();
    expect(matchAt('@', 0)).toBeNull();
    expect(matchAt('see @ ', 6)).toBeNull();
    expect(matchAt('mail me@host', 12)).toBeNull();
    expect(matchAt('@a b', 4)).toBeNull();
    expect(matchAt('hello', 5)).toBeNull();
    expect(matchAt('line one\n@x', 11)).toEqual({ query: 'x', start: 9 });
  });

  test('a cursor outside the text is clamped', () => {
    expect(matchAt('@ab', 99)).toEqual({ query: 'ab', start: 0 });
    expect(matchAt('@ab', -3)).toBeNull();
  });
});

describe('composer menus (DL-174)', () => {
  const noop = () => {};
  const rows = [
    { key: 'deploy', text: '/deploy', badge: 'skill' },
    { key: 'compact', text: '/compact' },
  ];

  test('the command menu shows the skill badge on a skill only', () => {
    const html = renderToStaticMarkup(
      <ComposerMenu id="m" label="Commands" rows={rows} current={0} onChoose={noop} />,
    );
    expect(html).toContain('aria-label="Commands"');
    expect(html).toMatch(/\/deploy<span[^>]*>skill<\/span>/);
    expect(html).toMatch(/\/compact<\/div>/);
    expect(html).toContain('aria-selected="true"');
  });

  test('the file menu shows the relative paths', () => {
    const html = renderToStaticMarkup(
      <ComposerMenu
        id="m"
        label="Files"
        rows={[{ key: 'src/a.ts', text: 'src/a.ts' }]}
        current={0}
        onChoose={noop}
      />,
    );
    expect(html).toContain('aria-label="Files"');
    expect(html).toContain('>src/a.ts</div>');
  });

  test('status rows are disabled options that the keys skip', () => {
    const html = renderToStaticMarkup(
      <ComposerMenu
        id="m"
        label="Files"
        rows={[
          { key: 'src/a.ts', text: 'src/a.ts' },
          { key: 'status', text: 'Searching…', status: true },
        ]}
        current={0}
        onChoose={noop}
      />,
    );
    expect(html).toMatch(
      /<div id="m-1" role="option" tabindex="-1" aria-disabled="true" aria-selected="false" class="fd-composer-status">Searching…<\/div>/,
    );
    expect(html).not.toMatch(/id="m-0"[^>]*aria-disabled/);
    for (const text of [
      'Type 2 or more letters to search files',
      'No matching files',
      'File search failed: boom',
    ]) {
      const only = renderToStaticMarkup(
        <ComposerMenu
          id="m"
          label="Files"
          rows={[{ key: 'status', text, status: true }]}
          current={0}
          onChoose={noop}
        />,
      );
      expect(only).toContain(`>${text}</div>`);
      expect(only).toContain('aria-disabled="true"');
    }
  });

  test('a failed search names the error message, or the station that did not answer', () => {
    expect(searchFailure(new Error('search is off'))).toBe('search is off');
    expect(searchFailure(new Error('x'.repeat(120)))).toBe('x'.repeat(120));
    expect(searchFailure(new Error('x'.repeat(121)))).toBe('the station did not answer');
    expect(searchFailure(new Error(''))).toBe('the station did not answer');
    expect(searchFailure('text')).toBe('the station did not answer');
    expect(searchFailure(undefined)).toBe('the station did not answer');
  });

  const composer = (extra: Partial<Parameters<typeof Composer>[0]> = {}) =>
    renderToStaticMarkup(
      <Composer
        enabled
        working={false}
        steer={false}
        nativeQueue={false}
        onSend={noop}
        {...extra}
      />,
    );

  test('a dead composer with onResume is a Resume bar, with no text box', () => {
    const bar = composer({ enabled: false, onResume: noop });
    expect(bar).toContain('<div class="fd-composer-resume">');
    expect(bar).toContain('<span class="fd-composer-note">The session ended.</span>');
    // The desktop header has the primary Resume: the bar repeats it as a plain button (§2.2).
    expect(bar).toMatch(/<button type="button">.*Resume<\/button>/);
    expect(bar).not.toContain('fd-primary');
    // On the phone the header shows no Resume: the bar has the one primary.
    expect(composer({ enabled: false, onResume: noop, phone: true })).toMatch(
      /<button type="button" class="fd-primary">.*Resume<\/button>/,
    );
    expect(bar).not.toContain('<textarea');
    expect(bar).not.toContain('Add image');
    // the reason of the session replaces the default text
    expect(composer({ enabled: false, onResume: noop, disabledReason: 'Gone.' })).toContain(
      '>Gone.</span>',
    );
    // without onResume, or while the composer is on, nothing changes
    expect(composer({ enabled: false })).toContain('<textarea');
    expect(composer({ enabled: false, onResume: null })).toContain('<textarea');
    // while a command is pending, the Resume button waits
    const busy = composer({ enabled: false, onResume: noop, resumeBusy: true });
    expect(busy).toContain('disabled=""');
    expect(busy).toContain('Resuming…');
    expect(composer({ enabled: true, onResume: noop })).toContain('<textarea');
  });

  test('a session without commands says so for a slash word only (DL-174)', () => {
    const draft = (text: string) => {
      const store = { getItem: () => text, setItem: () => {}, removeItem: () => {} };
      Object.defineProperty(globalThis, 'sessionStorage', { value: store, configurable: true });
      try {
        return composer({ draftKey: 'k' });
      } finally {
        Reflect.deleteProperty(globalThis, 'sessionStorage');
      }
    };
    const note = 'This session reports no commands. The text goes to the agent as a prompt.';
    expect(draft('/co')).toContain(note);
    expect(draft('hello')).not.toContain('This session reports no commands.');
    expect(draft('/co now')).not.toContain('This session reports no commands.');
  });

  test('the placeholder offers the commands when the session has some', () => {
    expect(composer({ slashCommands: ['init'] })).toContain('(/ for commands)');
    expect(composer({ slashCommands: [] })).not.toContain('(/ for commands)');
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
  // The Terminal summary is a ghost button (base.css `.fd-menu > summary.fd-ghost`).
  const terminal = /<details class="fd-menu"><summary class="fd-ghost" aria-label="Terminal">/;
  const match = new RegExp(`${terminal.source}[\\s\\S]*?<\\/details>`).exec(html);
  return match?.[0] ?? '';
}

describe('session header (DL-140)', () => {
  test('a rate limit of 80% or more shows a chip; 100% is a hazard (R4.2)', () => {
    const limited = (used: number) =>
      session({ rate_limits: [{ scope: 'five_hour', used_percent: used, resets_at: null }] });
    expect(header(limited(85))).toContain('data-tone="act">Limit 85% · 5-hour window</span>');
    expect(header(limited(100))).toContain(
      'data-tone="hazard">Limit reached · 5-hour window</span>',
    );
    expect(header(limited(79))).not.toContain('Limit ');
    expect(header(session())).not.toContain('Limit ');
  });

  test('a session of a batch has Compare batch in its menu (R1.4); others have none', () => {
    const inBatch = session({ batch: { batch_id: 'B1', index: 1, size: 3 } });
    expect(header(inBatch)).toContain('Compare batch');
    expect(header(session({ batch: { batch_id: 'B1', index: 1, size: 1 } }))).not.toContain(
      'Compare batch',
    );
    expect(header(session())).not.toContain('Compare batch');
  });

  test('a running session offers Interrupt and Stop', () => {
    const html = header(session());
    expect(html).toContain('aria-label="Session header"');
    expect(html).toContain('Interrupt');
    // Stop and Force stop are items of the session menu
    expect(html).toContain('aria-label="Session menu"');
    expect(html).toMatch(/<button[^>]*>(<svg[\s\S]*?<\/svg>)? Stop<\/button>/);
    // Force stop has its icon, so its text starts on the column of the other items.
    expect(html).toMatch(
      /class="fd-danger"[^>]*><svg[^>]*lucide-octagon-alert[^>]*>[\s\S]*?<\/svg> Force stop…/,
    );
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

  test('the mode fact shows the mode with its tone, and is absent without a mode or for default', () => {
    // The chip shows the sentence-case name of the mode; the title keeps the raw id.
    const label: Readonly<Record<string, string>> = {
      plan: 'Plan',
      acceptEdits: 'Accept edits',
      auto: 'Auto',
      dontAsk: 'Don&#x27;t ask',
      bypassPermissions: 'Bypass permissions',
      'danger-full-access': 'danger-full-access',
    };
    const fact = (mode: string) =>
      new RegExp(
        `<span class="fd-badge fd-sh-mode"(?: data-tone="([a-z]+)")? title="Permission mode: ${mode}">(<svg[^>]*>.*?</svg>)${label[mode]}</span>`,
      );
    const tone = (mode: string) => fact(mode).exec(header(session({ mode })))?.[1];
    expect(tone('plan')).toBe('plan');
    // DL-171: hazard red only for a mode that skips the approvals; the other modes have no tone.
    for (const risky of ['dontAsk', 'bypassPermissions', 'danger-full-access']) {
      expect(tone(risky)).toBe('hazard');
      // a risky mode has the shield-alert icon
      expect(fact(risky).exec(header(session({ mode: risky })))?.[2]).toContain('shield-alert');
    }
    // every chip has an icon, like the other place chips; a calm mode has no alert icon
    for (const calm of ['acceptEdits', 'auto', 'plan']) {
      expect(fact(calm).exec(header(session({ mode: calm })))?.[2]).not.toContain('shield-alert');
    }
    for (const calm of ['acceptEdits', 'auto']) {
      expect(fact(calm).test(header(session({ mode: calm })))).toBe(true);
      expect(tone(calm)).toBeUndefined();
    }
    expect(header(session({ mode: null }))).not.toContain('Permission mode');
    expect(header(session({ mode: 'default' }))).not.toContain('Permission mode');
  });

  test('the header shows plain facts, not pills, for the status, model and provider', () => {
    const words: Readonly<Record<string, string>> = {
      needs_you: 'Needs you',
      working: 'Working',
      verifying: 'Verifying',
      queued: 'Queued',
      idle: 'Idle',
      ended: 'Ended',
      lost: 'Lost',
      unreachable: 'Unreachable',
    };
    for (const [status, word] of Object.entries(words)) {
      const html = header(session({}, status as DeckSession['status']));
      expect(html).toContain(`<span class="fd-sh-status" data-status="${status}">`);
      expect(html).toContain(`${word}</span>`);
    }
    // The card says working for background work too: without a run no turn is active.
    const background = header(session({ current_run: null }, 'working'));
    expect(background).toContain(
      '<span class="fd-sh-status" data-status="working" title="Background work runs; no turn is active.">',
    );
    expect(background).toContain('Working in background</span>');
    expect(background).not.toContain('Interrupt');
    const html = header(session({ model: 'claude-sonnet-4-5' }));
    expect(html).toContain('class="fd-sh-model" data-family="sonnet"');
    expect(html).toContain('class="fd-sh-provider"');
    expect(html).not.toMatch(/class="fd-badge"[^>]*data-family/);
    expect(html).not.toMatch(/class="fd-badge"[^>]*data-provider/);
    expect(html).not.toContain(' · ');
  });

  test('the title falls back to the callsign, the model to "default model"', () => {
    const html = header(session({ title: null, callsign: 'otter', model: null }));
    expect(html).toContain('>otter</h2>');
    expect(html).toContain('default model');
  });

  test('the folder chip shows the home-short path (it fits its room by whole folders) and its title', () => {
    const cwd = `/home/someone/${'deep/'.repeat(20)}repo`;
    const html = header(session({ cwd, branch: 'main' }));
    expect(html).toContain(
      `<span class="fd-badge fd-sh-chip" data-mono="" data-folder="" title="${cwd}">`,
    );
    expect(html).toContain(`<bdi>~/${'deep/'.repeat(20)}repo</bdi>`);
    expect(html).toContain('main');
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
    expect(html).toContain('Set model');
    expect(html).toContain('Continue after a station restart');
  });

  test('the details block lists the facts of the session', () => {
    const html = header(
      session({ callsign: 'falcon', worktree: '/wt/fix', session_seq: 42, updated_at: Date.now() }),
    );
    expect(html).toContain('class="fd-sh-details"');
    expect(html).toContain('<dt>Session</dt><dd><span class="fd-sh-sid" title="s1">s1</span>');
    expect(html).toContain('aria-label="Copy the session id"');
    expect(html).toContain('<dt>Callsign</dt><dd>falcon</dd>');
    expect(html).toContain('<dt>Worktree</dt><dd>/wt/fix</dd>');
    expect(html).toContain('<dt>Updated</dt><dd>now</dd>');
    expect(html).toContain('<dt>Events</dt><dd>42</dd>');
    // A big count has a thousands separator.
    expect(header(session({ session_seq: 7405 }))).toContain('<dt>Events</dt><dd>7,405</dd>');
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

  test('the placeholder names the session by its callsign', () => {
    expect(composer({ callsign: 'otter-n1rr' })).toContain('placeholder="Message otter-n1rr…"');
    expect(composer({ callsign: 'otter-n1rr', slashCommands: ['init'] })).toContain(
      'placeholder="Message otter-n1rr… (/ for commands)"',
    );
    expect(composer({ callsign: null })).toContain('placeholder="Message the agent…"');
  });

  test('a disabled composer shows its reason instead of the follow-up note', () => {
    const reason = 'A terminal holds this session. Return it to board control to send prompts.';
    const html = composer({ enabled: false, working: true, disabledReason: reason });
    expect(html).toContain(reason);
    expect(html).not.toContain('The station holds a follow-up');
    expect(composer({ enabled: true, disabledReason: reason })).not.toContain(reason);
  });

  test('the buttons keep their texts and the textarea keeps its label', () => {
    expect(composer()).toMatch(
      /<\/svg> Send<span class="fd-kbd fd-composer-kbd" aria-hidden="true">↵<\/span><\/button>/,
    );
    expect(composer()).toContain('>Message</label>');
    expect(composer()).toContain('Add image');
    const working = composer({ working: true, steer: true, nativeQueue: false });
    expect(working).toContain('Steer now');
    expect(working).toContain('Queue follow-up');
    expect(working).toContain('The station holds a follow-up until the turn ends.');
    // §4.5: Steer now carries its ⌘↵ hint, the same as Queue follow-up carries ↵.
    expect(working).toContain(
      `aria-keyshortcuts="Meta+Enter Control+Enter">Steer now<span class="fd-kbd fd-composer-kbd" aria-hidden="true">${submitKeyHint()}</span></button>`,
    );
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

describe('the # comment menu of the composer (DL-179)', () => {
  const comments = [{ shortId: 'c-abc234', label: 'a.ts:1 hi' }];
  const render = (props: Partial<Parameters<typeof Composer>[0]> = {}) =>
    renderToStaticMarkup(
      <Composer enabled working={false} steer={false} nativeQueue onSend={() => {}} {...props} />,
    );
  test('with comments but no # word the composer shows no menu', () => {
    const html = render({ comments });
    expect(html).not.toContain('role="listbox"');
    expect(html).toContain('<textarea');
  });
  test('# is not a trigger of the other menus', () => {
    expect(matchHash('#ab', 3)).toEqual({ query: 'ab', start: 0 });
    expect(matchAt('#ab', 3)).toBeNull();
    expect(matchSlash('#ab', ['help'], [])).toBeNull();
  });
});
