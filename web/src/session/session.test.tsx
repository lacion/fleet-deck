import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { App } from '../App';
import type { DeckSession } from '../gen';
import type { Timeline } from '../runtime/state';
import { claudeCaps, codexCaps, minimalCaps } from '../testing/capabilities';
import { station } from '../testing/fleet';
import { summary } from '../testing/summary';
import {
  claudeCapabilities,
  nestedSubagents,
  recorded,
  recordedRequest,
} from '../testing/timeline';
import { commandId, outcomeOf, sessionActions } from './commands';

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

const view = (s: DeckSession, t?: Timeline, scopes = ['read', 'drive', 'approve']) =>
  renderToStaticMarkup(
    <App
      sessions={[s]}
      stations={[station()]}
      freshness="live"
      timelines={t ? { [JSON.stringify(['station-a', 'nested'])]: t } : {}}
      sessionActions={(k) => sessionActions(async () => ({}), k, scopes)}
      initialSession={key}
    />,
  );

describe('session view (P4-3)', () => {
  test('the tree nests child timelines with their status', () => {
    const html = view(session(), timeline());
    expect(html).toContain('<h2>Review</h2>');
    expect(html).toContain('data-running-subagents="true">3</span>');
    expect(html).toMatch(/data-subagent="node-1".*Explore the code · .*running/);
    // node-4 is a child of node-1; node-2 is the next sibling of node-1.
    const one = html.indexOf('data-subagent="node-1"');
    const four = html.indexOf('data-subagent="node-4"');
    const two = html.indexOf('data-subagent="node-2"');
    expect(one).toBeLessThan(four);
    expect(four).toBeLessThan(two);
    expect(html).toContain('Approval: Bash · waiting for an answer');
  });

  test('a recorded background subagent shows its background state', () => {
    const events = recorded('subagent').filter((e) => e.session_seq <= 8);
    const html = view(session(), timeline(events));
    expect(html).toContain('running in the background');
    expect(html).toContain('No events from this subagent.');
  });

  test('controls follow the capabilities, the run and the scopes', () => {
    const working = view(session(), timeline());
    expect(working).toContain('Steer now');
    expect(working).toContain('Queue follow-up');
    expect(working).toContain('Interrupt');
    expect(working).toContain('Mode (most restrictive first)');
    expect(working).toContain('Model (now unknown)');
    expect(working).toContain('aria-pressed="true" disabled="">default</button>');

    const idle = view(session({ current_run: null }), timeline());
    expect(idle).not.toContain('Steer now');
    expect(idle).toContain('>Send</button>');

    const none = view(session({ capabilities: null }), timeline());
    expect(none).not.toContain('Interrupt');
    expect(none).not.toContain('Mode (most restrictive first)');
    expect(none).not.toContain('Steer now');
    expect(none).not.toContain('Set model');

    const caps = { ...claudeCapabilities(), steer: false, set_mode: false };
    const noSteer = view(session({ capabilities: caps }), timeline());
    expect(noSteer).not.toContain('Steer now');
    expect(noSteer).not.toContain('Mode (most restrictive first)');
  });

  test('a Codex session shows the controls of its capabilities (P5-3)', () => {
    const codex = view(session({ capabilities: codexCaps(), mode: 'untrusted/workspace-write' }));
    expect(codex).toContain('Steer now');
    expect(codex).toContain('Interrupt');
    expect(codex).toContain('Set model');
    expect(codex).toContain('>untrusted/read-only</button>');
    expect(codex).toContain('aria-pressed="true" disabled="">untrusted/workspace-write</button>');
    expect(codex).not.toContain('>acceptEdits</button>');
    // No native follow-up queue: the station holds the follow-up.
    expect(codex).toContain('Queue follow-up');
    expect(codex).toContain('The station holds a follow-up until the turn ends.');
    const claude = view(session({ capabilities: claudeCaps() }));
    expect(claude).not.toContain('The station holds a follow-up');
    expect(claude).toContain('>acceptEdits</button>');
  });

  test('a provider without optional capabilities gets prompt and follow-up only', () => {
    const req = {
      request_id: 'req-1',
      node_id: null,
      request_kind: 'command' as const,
      title: 'Run ls',
      details: 'ls',
      input: null,
      options: [{ option_id: 'allow', label: 'Allow' }],
      response: 'rpc_reply' as const,
      requested_at: 1,
    };
    const html = view(session({ capabilities: minimalCaps(), open_requests: [req] }));
    expect(html).not.toContain('Interrupt');
    expect(html).not.toContain('Steer now');
    expect(html).not.toContain('Mode (most restrictive first)');
    expect(html).not.toContain('Set model');
    expect(html).toContain('Queue follow-up');
    expect(html).toContain('Run ls');
    expect(html).toContain('This provider has no approval controls.');
    expect(html).not.toContain('>Allow</button>');
    const answerable = view(session({ capabilities: codexCaps(), open_requests: [req] }));
    expect(answerable).toContain('>Allow</button>');
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
    expect(codex).toMatch(/<button type="submit" disabled="">Send answer<\/button>/);
    expect(codex).not.toContain('>Answer</button>');
    const claude = view(session({ open_requests: [recordedRequest('ask_user_question')] }));
    expect(claude).toContain('<legend>Color: Which color?</legend>');
    expect(claude).toContain('>Deny</button>');
    expect(claude).not.toContain('>Allow once</button>');
  });

  test('a stale board or an ended session disables every control', () => {
    const stale = renderToStaticMarkup(
      <App
        sessions={[session()]}
        stations={[station()]}
        freshness="stale"
        sessionActions={(k) => sessionActions(async () => ({}), k, ['drive'])}
        initialSession={key}
      />,
    );
    expect(stale).toContain('Controls are off until the board and the station are live.');
    expect(stale).toContain('Loading the timeline…');
    const ended = view(session({ state: 'exited', current_run: null }), timeline());
    expect(ended).toMatch(/<textarea id="fd-composer-text" rows="3" disabled="">/);
  });

  test('rename works on an ended session and needs drive and a live board (DL-095)', () => {
    const ended = view(session({ state: 'exited', current_run: null }), timeline());
    expect(ended).toMatch(/<button type="button">Rename<\/button>/);
    const readOnly = view(session(), timeline(), ['read']);
    expect(readOnly).toMatch(/<button type="button" disabled="">Rename<\/button>/);
    const stale = renderToStaticMarkup(
      <App
        sessions={[session()]}
        stations={[station()]}
        freshness="stale"
        sessionActions={(k) => sessionActions(async () => ({}), k, ['drive'])}
        initialSession={key}
      />,
    );
    expect(stale).toMatch(/<button type="button" disabled="">Rename<\/button>/);
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
    const req = (id: string, at: number) => ({
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
    const html = renderToStaticMarkup(
      <App
        sessions={[session({ open_requests: [req('late', 9), req('early', 2)] })]}
        stations={[station()]}
        freshness="live"
      />,
    );
    expect(html.indexOf('Run early')).toBeLessThan(html.indexOf('Run late'));
    expect(html).toContain('aria-label="Answer Run early in Review"');
    const empty = renderToStaticMarkup(<App sessions={[session()]} freshness="live" />);
    expect(empty).toContain('Nothing waits for you.');
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
