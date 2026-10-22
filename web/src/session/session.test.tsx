import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { App } from '../App';
import type {
  DeckSession,
  Event,
  EventEnvelope,
  ShellInfo,
  StationInfo,
  StationView,
} from '../gen';
import type { Freshness, Timeline } from '../runtime/state';
import type { ShellActions } from '../shell/shell';
import { createTerminalStore } from '../terminal/store';
import { claudeCaps, codexCaps, minimalCaps } from '../testing/capabilities';
import { station } from '../testing/fleet';
import { summary } from '../testing/summary';
import {
  claudeCapabilities,
  envelopes,
  nestedSubagents,
  recorded,
  recordedRequest,
} from '../testing/timeline';
import { commandId, outcomeOf, sessionActions } from './commands';
import { type PaneTab, readFilesOpen, SessionView } from './SessionView';

const key = { station_id: 'station-a', session_id: 'nested' };

function session(extra: Partial<DeckSession['summary']> = {}): DeckSession {
  return {
    station_id: 'station-a',
    status: 'working',
    summary: summary('nested', 'working', 1, {
      title: 'Review',
      capabilities: claudeCapabilities(),
      mode: 'default',
      current_run: 'run-1',
      ...extra,
    }),
  };
}

const timeline = (events = nestedSubagents(10)): Timeline => ({
  incarnation: 'inc',
  cursor: 0,
  events,
  freshness: 'live',
  error: null,
  ready: true,
});

type ViewOptions = {
  readonly scopes?: string[];
  readonly freshness?: Freshness;
  readonly tab?: PaneTab;
  readonly shells?: ShellActions;
  /** Offer the file service (the Files tab, or the side panel on a desktop). */
  readonly files?: boolean;
  /** The side panel is only on the desktop layout; the tablet has the Files tab. */
  readonly layout?: 'desktop' | 'tablet' | 'phone';
  /** The stations the board knows (default: one station without a report). */
  readonly stations?: readonly StationView[];
  /** Give the view the terminal features of an admin board (needs `shells`). */
  readonly terminals?: boolean;
};

const view = (s: DeckSession, t?: Timeline, options: ViewOptions = {}) =>
  renderToStaticMarkup(
    <SessionView
      session={s}
      stations={options.stations ?? [station()]}
      freshness={options.freshness ?? 'live'}
      timeline={t}
      actions={sessionActions(
        async () => ({}),
        key,
        options.scopes ?? ['read', 'drive', 'approve'],
        undefined,
        options.files ? async () => ({}) : undefined,
      )}
      shells={options.shells}
      terminals={options.terminals ? createTerminalStore(null) : undefined}
      watch={options.terminals ? { get: () => null, subscribe: () => () => {} } : undefined}
      layout={options.layout ?? 'desktop'}
      tab={options.tab ?? 'chat'}
    />,
  );

const at = (n: number, event: EventEnvelope['event']): EventEnvelope => ({
  station_seq: n,
  session_id: 'nested',
  session_seq: n,
  at: n,
  proto_version: 1,
  raw_ref: null,
  event,
});

const shells = (): ShellActions => ({
  list: async (): Promise<ShellInfo[]> => [],
  open: async () => 'shell-1',
  resize: async () => {},
  close: async () => {},
  handover: async () => 'tui-1',
  release: async () => {},
  paste: async () => '/tmp/x.png',
  socketPath: () => '/x',
});

const request = (id: string, at = 1) => ({
  request_id: id,
  node_id: null,
  request_kind: 'command' as const,
  title: `Run ${id}`,
  details: 'ls',
  input: null,
  options: [{ option_id: 'allow', label: 'Allow' }],
  response: 'rpc_reply' as const,
  requested_at: at,
});

describe('session pane (DL-140)', () => {
  test('the header model selector takes the models of the summary (DL-151)', () => {
    const models = [{ value: 'opus', label: 'Opus', description: null }];
    const html = view(session({ models }), timeline());
    expect(html).toContain('aria-label="Model"');
    expect(html).toContain('<option value="opus">Opus</option>');
    expect(html).toContain('Other…');
    expect(view(session(), timeline())).not.toContain('<option value="opus">Opus</option>');
  });

  test('the header shows the title and the pane has the three views', () => {
    const html = view(session(), timeline());
    expect(html).toContain('<section class="fd-pane" aria-label="Session" data-layout="desktop">');
    expect(html).toContain('<h2 class="fd-sh-title">Review</h2>');
    expect(html).toContain('role="tablist" aria-label="Session views"');
    expect(html).toMatch(/role="tab"[^>]*aria-selected="true"[^>]*tabindex="0">Chat</);
    expect(html).toMatch(/role="tab"[^>]*aria-selected="false"[^>]*tabindex="-1">Changes</);
    expect(html).toContain('role="tabpanel"');
    // Files needs the file service of the actions.
    expect(html).not.toContain('>Files</button>');
    const withFiles = view(session(), timeline(), { files: true, layout: 'tablet' });
    expect(withFiles).toContain('>Files</button>');
    expect(withFiles).not.toContain('aria-label="Files panel"');
  });

  test('on a desktop the Files side panel is closed by default and has a toggle', () => {
    // no stored choice: the panel is closed, the Files tab is offered, the toggle is not pressed
    const closed = view(session(), timeline(), { files: true });
    expect(closed).not.toContain('aria-label="Files panel"');
    expect(closed).toMatch(/<button[^>]*aria-label="Files"[^>]*aria-pressed="false"/);
    // a stored `1` opens it
    const store = { getItem: () => '1', setItem: () => {}, removeItem: () => {} };
    Object.defineProperty(globalThis, 'localStorage', { value: store, configurable: true });
    let html: string;
    try {
      html = view(session(), timeline(), { files: true });
    } finally {
      Reflect.deleteProperty(globalThis, 'localStorage');
    }
    expect(html).toContain('<aside class="fd-pane-side" aria-label="Files panel">');
    expect(html).toMatch(/<button[^>]*aria-label="Files"[^>]*aria-pressed="true"/);
    expect(html).not.toMatch(/role="tab"[^>]*>Files</);
    // without the file service there is neither a panel nor a toggle
    const none = view(session(), timeline());
    expect(none).not.toContain('aria-label="Files panel"');
    expect(none).not.toContain('aria-label="Files"');
    // the phone and the tablet have no side panel
    expect(view(session(), timeline(), { files: true, layout: 'phone' })).not.toContain(
      'aria-label="Files panel"',
    );
  });

  test('the Files panel reads closed unless a `1` is stored', () => {
    const store = (value: string | null) => ({ getItem: () => value });
    expect(readFilesOpen(store(null))).toBe(false);
    expect(readFilesOpen(store('0'))).toBe(false);
    expect(readFilesOpen(store('1'))).toBe(true);
    expect(
      readFilesOpen({
        getItem: () => {
          throw new Error('blocked');
        },
      }),
    ).toBe(false);
  });

  test('the Changes tab shows the count of finalized turns', () => {
    const finalized = (turn: string): Event => ({
      kind: 'turn.finalized',
      turn_id: turn,
      checkpoint: {
        worktree_id: 'wt',
        scope: 'tracked',
        start: { state: 'available', commit: `${turn}-s` },
        end: { state: 'available', commit: `${turn}-e` },
      },
      diff_summary: { files_changed: 1, insertions: 2, deletions: 1 },
    });
    const events = envelopes('nested', [finalized('t1'), finalized('t2')]);
    const html = view(session({ capabilities: codexCaps() }), timeline(events));
    expect(html).toMatch(/>Changes <span class="fd-count">2<\/span><\/button>/);
    const changes = view(session({ capabilities: codexCaps() }), timeline(events), {
      tab: 'changes',
    });
    expect(changes).toContain('aria-label="Turns"');
    expect(changes).toContain('data-tab="changes"');
    expect(changes).not.toContain('aria-label="Composer"');
  });

  test('the Changes tab badge counts the changed files and is absent for 0', () => {
    const finalized = (turn: string, files: number): Event => ({
      kind: 'turn.finalized',
      turn_id: turn,
      checkpoint: {
        worktree_id: 'wt',
        scope: 'tracked',
        start: { state: 'available', commit: `${turn}-s` },
        end: { state: 'available', commit: `${turn}-e` },
      },
      diff_summary: { files_changed: files, insertions: files, deletions: 0 },
    });
    const some = envelopes('nested', [finalized('t1', 1), finalized('t2', 0), finalized('t3', 2)]);
    const html = view(session({ capabilities: codexCaps() }), timeline(some));
    // No spawn base: the files of the kept turns (1 + 0 + 2), not the count of turns.
    expect(html).toMatch(/>Changes <span class="fd-count">3<\/span><\/button>/);
    const none = envelopes('nested', [finalized('t1', 0)]);
    const empty = view(session({ capabilities: codexCaps() }), timeline(none));
    expect(empty).toMatch(
      /role="tab"[^>]*aria-selected="false"[^>]*tabindex="-1">Changes<\/button>/,
    );
    expect(empty).not.toContain('fd-count">0');
  });

  test('the Changes tab says so when no turn has finished', () => {
    const html = view(session(), timeline(), { tab: 'changes' });
    expect(html).toContain(
      '<p class="fd-note">No finished turns yet. Changes appear here when a turn ends.</p>',
    );
    expect(html).not.toContain('aria-label="Turns"');
  });

  test('the Files tab falls back to the chat without a file service', () => {
    const html = view(session(), timeline(), { tab: 'files' });
    expect(html).toContain('data-tab="chat"');
    expect(html).toMatch(/aria-selected="true"[^>]*tabindex="0">Chat</);
  });

  test('the Files tab says when the board is not live or the drive scope is missing', () => {
    const note = 'Files are available when the board is live and you have the drive scope.';
    const withFiles = (freshness: Freshness, scopes: string[]) =>
      view(session(), timeline(), {
        freshness,
        scopes,
        files: true,
        tab: 'files',
        layout: 'tablet',
      });
    const ready = withFiles('live', ['read', 'drive']);
    expect(ready).toContain('aria-label="Files"');
    expect(ready).not.toContain(note);
    const readOnly = withFiles('live', ['read']);
    // Without drive the file service is offered but not usable.
    expect(readOnly).toContain(note);
    expect(withFiles('stale', ['read', 'drive'])).toContain(note);
  });

  test('a held session shows the notice, the release button and the composer reason', () => {
    const held = session({
      state: 'exited',
      current_run: null,
      terminal: { shell_id: 'tui-7', pid: 42, since: 1, reason: 'owner handover' },
    });
    const html = view(held, timeline(), { shells: shells() });
    expect(html).toContain('role="note"');
    expect(html).toContain('A terminal holds this session (shell tui-7).');
    expect(html).toContain('owner handover');
    expect(html).toContain('>Release the hold</button>');
    expect(html).toContain(
      'A terminal holds this session. Return it to board control to send prompts.',
    );
    expect(html).toMatch(/<textarea id="fd-composer-text" rows="1" disabled=""/);
    // Without the admin scope the board offers no release.
    expect(view(held, timeline())).not.toContain('Release the hold');
    expect(view(session(), timeline(), { shells: shells() })).not.toContain('Release the hold');
  });

  test('a station with allow_shell = false gives no Open in terminal and says why (DL-141)', () => {
    const info = (allowShell: boolean | null): StationInfo => ({
      name: 'Alpha',
      incarnation: 'inc',
      head: 0,
      live_sessions: 0,
      version: '0.2.0',
      max_sessions: 4,
      allow_shell: allowShell,
      providers: [],
    });
    const open = (allowShell: boolean | null) =>
      view(session(), timeline(), {
        shells: shells(),
        terminals: true,
        stations: [station('station-a', { info: info(allowShell), info_at: 1 })],
      });
    const off = open(false);
    expect(off).not.toContain('Open in terminal…');
    expect(off).toContain(
      'Shells are off on this station. Set allow_shell = true in station.toml to open a terminal.',
    );
    for (const allowed of [true, null]) {
      const html = open(allowed);
      expect(html).toContain('Open in terminal…');
      expect(html).not.toContain('Shells are off');
    }
  });

  test('an ended session says how to send prompts again', () => {
    const html = view(session({ state: 'exited', current_run: null }), timeline());
    expect(html).toContain('The session ended. Resume it to send prompts.');
    // Claude can resume: the composer becomes a Resume bar, with no text box
    expect(html).toContain('class="fd-composer-resume"');
    expect(html).not.toContain('<textarea');
    // without the resume capability the composer stays, off, with the same text
    const stuck = view(
      session({ state: 'exited', current_run: null, capabilities: minimalCaps() }),
      timeline(),
    );
    expect(stuck).toContain('The session ended. Resume it to send prompts.');
    expect(stuck).toMatch(/<textarea id="fd-composer-text" rows="1" disabled=""/);
  });

  test('a stale board turns the controls off and says why', () => {
    const html = view(session(), undefined, { freshness: 'stale' });
    expect(html).toContain('Controls are off until the board and the station are live.');
    expect(html).toMatch(/<textarea id="fd-composer-text" rows="1" disabled=""/);
  });

  test('a session without the drive scope says why', () => {
    const html = view(session(), timeline(), { scopes: ['read'] });
    expect(html).toContain('Sending prompts needs the drive scope.');
  });

  test('the slash commands of the session reach the composer', () => {
    const events = [
      at(1, { kind: 'session.slash_commands', commands: ['init', 'compact'], skills: [] }),
    ];
    const html = view(session(), timeline(events));
    expect(html).toContain('(/ for commands)');
    expect(view(session(), timeline([]))).not.toContain('(/ for commands)');
  });

  test('the commands of the summary reach the composer without a timeline event (DL-174)', () => {
    const html = view(session({ slash_commands: ['init'], skills: ['init'] }), timeline([]));
    expect(html).toContain('(/ for commands)');
  });

  test('the chat nests the subagents and shows the open approval', () => {
    const html = view(session(), timeline());
    expect(html).toContain('aria-label="Conversation"');
    expect(html).toMatch(
      /fd-chat-subagent[\s\S]*fd-chat-subagent[\s\S]*Waiting for your answer · /,
    );
    expect(html.match(/fd-chat-subagent/g)?.length).toBeGreaterThanOrEqual(3);
  });

  test('a recorded background subagent shows its background state', () => {
    const events = recorded('subagent').filter((e) => e.session_seq <= 12);
    const html = view(session(), timeline(events));
    expect(html).toContain('running in the background');
  });

  test('the composer follows the capabilities and the run', () => {
    const working = view(session(), timeline());
    expect(working).toContain('Steer now');
    expect(working).toContain('Queue follow-up');

    const idle = view(session({ current_run: null }), timeline());
    expect(idle).not.toContain('Steer now');
    expect(idle).toMatch(
      /<\/svg> Send<span class="fd-kbd fd-composer-kbd" aria-hidden="true">↵<\/span><\/button>/,
    );

    const none = view(session({ capabilities: null }), timeline());
    expect(none).not.toContain('Steer now');

    const caps = { ...claudeCapabilities(), steer: false, set_mode: false };
    const noSteer = view(session({ capabilities: caps }), timeline());
    expect(noSteer).not.toContain('Steer now');
  });

  test('a Codex session shows the controls of its capabilities (P5-3)', () => {
    const codex = view(session({ capabilities: codexCaps(), mode: 'untrusted/workspace-write' }));
    expect(codex).toContain('Steer now');
    // The modes are menu rows: a mode without a name keeps its raw id; the current one is checked.
    expect(codex).toContain('</span>untrusted/read-only</button>');
    expect(codex).toMatch(
      /aria-pressed="true" disabled=""><span class="fd-mode-lead"><\/span>untrusted\/workspace-write<svg[^>]*lucide-check/,
    );
    expect(codex).not.toContain('Accept edits</button>');
    // No native follow-up queue: the station holds the follow-up.
    expect(codex).toContain('Queue follow-up');
    expect(codex).toContain('The station holds a follow-up until the turn ends.');
    const claude = view(session({ capabilities: claudeCaps() }));
    expect(claude).not.toContain('The station holds a follow-up');
    expect(claude).toContain('</span>Accept edits</button>');
  });

  test('a provider without optional capabilities gets prompt and follow-up only', () => {
    const req = request('req-1');
    const html = view(session({ capabilities: minimalCaps(), open_requests: [req] }));
    expect(html).not.toContain('Steer now');
    expect(html).not.toContain('Set model');
    expect(html).toContain('Queue follow-up');
    expect(html).toContain('Run req-1');
    expect(html).toContain('This provider has no approval controls.');
    expect(html).not.toContain('>Allow<');
    const answerable = view(session({ capabilities: codexCaps(), open_requests: [req] }));
    // The decision primary has no key hint: the session page has no a/d keys.
    expect(answerable).toContain('>Allow</button>');
    // The dock sits above the composer.
    expect(answerable.indexOf('Needs you')).toBeLessThan(
      answerable.indexOf('aria-label="Composer"'),
    );
  });

  test('a question request gets an answer form for both providers (DL-068)', () => {
    const codex = view(
      session({
        capabilities: codexCaps(),
        open_requests: [recordedRequest('codex-question_rpc')],
      }),
    );
    expect(codex).toContain('aria-label="Answer: Which color do you prefer?"');
    expect(codex).toContain('<legend>Color: Which color do you prefer?</legend>');
    expect(codex).toMatch(/type="radio"[^>]*value="Red"/);
    expect(codex).toContain('Other answer');
    // Nothing is chosen yet: the answer cannot be sent.
    expect(codex).toMatch(
      /<button type="submit" class="fd-decide" disabled="">Send answer<\/button>/,
    );
    expect(codex).not.toContain('>Answer</button>');
    const claude = view(session({ open_requests: [recordedRequest('ask_user_question')] }));
    expect(claude).toContain('<legend>Color: Which color?</legend>');
    expect(claude).toContain('>Deny</button>');
    expect(claude).not.toContain('>Allow once</button>');
  });

  test('rename works on an ended session and needs drive and a live board (DL-095)', () => {
    const rename = (html: string) => html.match(/<button[^>]*aria-label="Rename"[^>]*>/)?.[0] ?? '';
    const ended = view(session({ state: 'exited', current_run: null }), timeline());
    expect(rename(ended)).not.toContain('disabled');
    expect(rename(view(session(), timeline(), { scopes: ['read'] }))).toContain('disabled=""');
    expect(rename(view(session(), timeline(), { freshness: 'stale' }))).toContain('disabled=""');
  });

  test('a rename posts the title or a clear', async () => {
    const posts: [string, unknown][] = [];
    const actions = sessionActions(
      async (path, body) => {
        posts.push([path, body]);
        return { state: 'done', receipt: { state: 'accepted' } };
      },
      key,
      ['drive'],
      () => 'c1',
    );
    await actions.rename('Docs pass');
    await actions.rename(null);
    expect(posts).toEqual([
      [
        '/api/commands/session.rename',
        {
          station: 'station-a',
          command_id: 'c1',
          command: { op: 'rename', session_id: 'nested', title: 'Docs pass' },
        },
      ],
      [
        '/api/commands/session.rename',
        {
          station: 'station-a',
          command_id: 'c1',
          command: { op: 'rename', session_id: 'nested', title: null },
        },
      ],
    ]);
  });

  test('the needs-you rail lists open approvals oldest first', () => {
    const html = renderToStaticMarkup(
      <App
        sessions={[session({ open_requests: [request('late', 9), request('early', 2)] })]}
        stations={[station()]}
        freshness="live"
      />,
    );
    // the card shows the first request; the rail orders by age
    const rail = html.slice(html.indexOf('aria-label="Needs you rail"'));
    expect(rail.indexOf('Run early')).toBeGreaterThan(-1);
    expect(rail.indexOf('Run early')).toBeLessThan(rail.indexOf('Run late'));
    expect(rail).toContain('aria-label="Answer Run early in Review"');
    const empty = renderToStaticMarkup(<App sessions={[session()]} freshness="live" />);
    // no request: no rail, and the fleet header says all clear
    expect(empty).not.toContain('Needs you rail');
    expect(empty).toContain('All clear');
  });
});

describe('auto-continue (ADR-007, DL-127)', () => {
  test('the toggle is off by default and needs drive and a live board', () => {
    const html = view(session(), timeline());
    expect(html).toMatch(/<input type="checkbox"\/> Continue after a station restart/);
    const on = view(session({ auto_continue: true }), timeline());
    expect(on).toMatch(/<input type="checkbox" checked=""\/> Continue after a station restart/);
    const readOnly = view(session(), timeline(), { scopes: ['read'] });
    expect(readOnly).toMatch(
      /<input type="checkbox" disabled=""\/> Continue after a station restart/,
    );
  });

  test('a continuation run and a skipped continuation are shown', () => {
    const events = [
      at(1, {
        kind: 'run.started',
        run_id: 'r1',
        input_summary: 'Continue',
        origin: 'continuation',
      }),
      at(2, { kind: 'session.auto_continue_skipped', reason: 'newer_owner_work' }),
    ];
    const html = view(session(), timeline(events));
    // backlog B2: a muted line, no bubble and no label
    expect(html).toContain(
      '<p class="fd-chat-line" data-origin="continuation">The station continued the session after a restart.</p>',
    );
    expect(html).not.toContain('fd-chat-run-label');
    expect(html).not.toContain('fd-chat-bubble');
    expect(html).toContain('Auto-continue skipped: newer owner work');
  });

  test('the toggle posts session.set_auto_continue', async () => {
    const posts: [string, unknown][] = [];
    const actions = sessionActions(
      async (path, body) => {
        posts.push([path, body]);
        return { state: 'done', receipt: { state: 'accepted' } };
      },
      key,
      ['drive'],
      () => 'c1',
    );
    await actions.setAutoContinue(true);
    expect(posts).toEqual([
      [
        '/api/commands/session.set_auto_continue',
        {
          station: 'station-a',
          command_id: 'c1',
          command: { op: 'set_auto_continue', session_id: 'nested', enabled: true },
        },
      ],
    ]);
  });
});

describe('session commands', () => {
  test('a command id is fresh and fits the deck rule', () => {
    const a = commandId();
    expect(a).toMatch(/^[0-9A-Za-z_-]{1,64}$/);
    expect(commandId()).not.toBe(a);
    expect(commandId(() => 0)).toBe(`b${'0'.repeat(24)}`);
  });

  test('each action posts one command to its method', async () => {
    const posts: [string, unknown][] = [];
    const ids = ['c1', 'c2', 'c3', 'c4', 'c5', 'c6', 'c7'];
    const actions = sessionActions(
      async (path, body) => {
        posts.push([path, body]);
        return { state: 'done', receipt: { state: 'accepted' } };
      },
      key,
      ['read', 'drive'],
      () => ids.shift() ?? 'x',
    );
    expect([actions.canDrive, actions.canApprove]).toEqual([true, false]);
    await actions.send('prompt', 'hi');
    await actions.send('steer', 'focus');
    await actions.send('follow_up', 'next');
    await actions.interrupt();
    await actions.answer('req-1', 'deny');
    expect(await actions.setMode('plan')).toEqual({ ok: true, text: 'Done.' });
    await actions.setModel('haiku');
    expect(posts.map(([p]) => p)).toEqual([
      '/api/commands/session.prompt',
      '/api/commands/session.steer',
      '/api/commands/session.follow_up',
      '/api/commands/session.interrupt',
      '/api/commands/approval.answer',
      '/api/commands/session.set_mode',
      '/api/commands/session.set_model',
    ]);
    expect(posts[6]?.[1]).toMatchObject({
      command: { op: 'set_model', session_id: 'nested', model: 'haiku' },
    });
    expect(posts[4]?.[1]).toEqual({
      station: 'station-a',
      command_id: 'c5',
      command: {
        op: 'answer',
        session_id: 'nested',
        request_id: 'req-1',
        decision: { option_id: 'deny', updated_input: null, text: null },
      },
    });
  });

  test('outcomes name refusals, unknown delivery and failures', async () => {
    expect(outcomeOf({ state: 'pending' }).text).toBe(
      'Sent. The station has not confirmed it yet.',
    );
    expect(
      outcomeOf({
        state: 'done',
        receipt: { state: 'rejected', reject: { message: 'No run is active.' } },
      }),
    ).toEqual({ ok: false, text: 'Refused: No run is active.' });
    expect(outcomeOf({ state: 'ambiguous' }).ok).toBe(false);
    expect(outcomeOf({ state: 'failed', reason: 'expired' }).text).toBe('Not delivered: expired');
    expect(outcomeOf(null).ok).toBe(false);
    expect(outcomeOf({ state: 'odd' }).ok).toBe(false);
    const failing = sessionActions(
      async () => {
        throw new Error('The deck is not reachable.');
      },
      key,
      ['drive'],
    );
    expect(await failing.interrupt()).toEqual({ ok: false, text: 'The deck is not reachable.' });
  });
});
