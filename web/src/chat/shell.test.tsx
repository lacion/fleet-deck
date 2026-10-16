// B8: the shell card, its states, the release rules, the pending-result chips.
import { describe, expect, test } from 'bun:test';
import { useState } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import type { Event } from '../gen';
import { sessionActions } from '../session/commands';
import { buildTimeline, type Item } from '../session/timeline';
import { envelopes } from '../testing/timeline';
import { applyChatFilter, compactRuns, DEFAULT_FILTER } from './filter';
import { ChatView } from './ChatView';
import {
  effectIdOf,
  isShellTool,
  ReleaseDialog,
  releaseOffered,
  type ShellEnv,
  ShellCard,
  shellState,
  unfoldShells,
} from './ShellCard';
import { pendingResults, ShellChips, useShellChips } from './ShellChips';

type Tool = Extract<Item, { type: 'tool' }>;

const started = (id: string, command: string): Event => ({
  kind: 'tool.started',
  node_id: `shell-${id}`,
  parent_node_id: null,
  tool_kind: 'shell',
  title: 'Shell command',
  input_summary: command,
  files: [],
  provider_refs: {},
});
const output = (id: string, chunk: string): Event => ({
  kind: 'tool.output',
  node_id: `shell-${id}`,
  chunk,
  truncated: false,
});
const completed = (id: string, shell: string | null, exit: number | null, ok = true): Event => ({
  kind: 'tool.completed',
  node_id: `shell-${id}`,
  status: ok ? 'ok' : 'failed',
  files: [],
  exit_code: exit,
  provider_refs: shell === null ? {} : { shell },
});
const run = (origin: 'board' | 'provider' = 'board'): Event => ({
  kind: 'run.started',
  run_id: 'run-1',
  input_summary: 'go',
  origin,
});

const items = (events: Event[]): Item[] => buildTimeline(envelopes('s', events)).items.slice();
const tools = (events: Event[]): Tool[] =>
  items(events).filter((i): i is Tool => i.type === 'tool');
const only = (events: Event[]): Tool => {
  const [tool] = tools(events);
  if (tool === undefined) throw new Error('no tool');
  return tool;
};

const env = (extra: Partial<ShellEnv> = {}): ShellEnv => ({
  canStop: true,
  onStop: () => {},
  canRelease: true,
  ended: false,
  onRelease: async () => ({ ok: true, text: 'Done.' }),
  ...extra,
});

describe('the timeline keeps the shell ref (B8)', () => {
  test('the output folds into the item and provider_refs.shell is kept', () => {
    const tool = only([started('e1', 'pwd'), output('e1', '/w\n'), completed('e1', 'done', 0)]);
    expect(tool.output).toBe('/w\n');
    expect(tool.shell).toBe('done');
    expect(tool.exit_code).toBe(0);
  });

  test('a command without a completion has no shell ref', () => {
    expect(only([started('e1', 'sleep 9')]).shell).toBeUndefined();
  });
});

describe('shell state texts', () => {
  test('every state', () => {
    const text = (events: Event[]) => shellState(only(events)).text;
    expect(text([started('e', 'x')])).toBe('Running…');
    expect(text([started('e', 'x'), completed('e', 'done', 0)])).toBe('exit 0');
    expect(text([started('e', 'x'), completed('e', 'done', 2, false)])).toBe('exit 2');
    expect(text([started('e', 'x'), completed('e', 'cancelled', null, false)])).toBe('Cancelled');
    expect(text([started('e', 'x'), completed('e', 'timeout', null, false)])).toBe('Timed out');
    expect(text([started('e', 'x'), completed('e', 'unknown', null, false)])).toBe(
      'Unknown: the station restarted during the command',
    );
    expect(text([started('e', 'x'), completed('e', 'released', null, false)])).toBe('Released');
  });

  test('a card that the session left unfinished says so', () => {
    const lost: Event = { kind: 'session.state', state: 'lost', detail: null };
    expect(shellState(only([started('e', 'x'), lost])).text).toBe('Ended with the session');
  });

  test('a Codex item takes its state from status and exit code', () => {
    const codex: Event[] = [
      { ...started('x', 'ls'), node_id: 'item-9' } as Event,
      { kind: 'tool.completed', node_id: 'item-9', status: 'failed', files: [], exit_code: 3 },
    ];
    const tool = only(codex);
    expect(isShellTool(tool)).toBe(true);
    expect(effectIdOf(tool)).toBeNull();
    expect(shellState(tool).text).toBe('exit 3');
  });
});

describe('the shell card', () => {
  const card = (events: Event[], e: ShellEnv | null = env(), staleMs?: number) =>
    renderToStaticMarkup(
      <ShellCard
        tool={only(events)}
        env={e ?? undefined}
        {...(staleMs === undefined ? {} : { staleMs })}
      />,
    );

  test('shows the title, the command, the output and the state', () => {
    const html = card([started('e1', 'ls -la'), output('e1', 'a\nb'), completed('e1', 'done', 0)]);
    expect(html).toContain('Shell command');
    expect(html).toContain('ls -la');
    expect(html).toContain('a\nb');
    expect(html).toContain('exit 0');
    expect(html).not.toContain('Stop');
  });

  test('a running card has Stop for the owner and none without the right', () => {
    expect(card([started('e1', 'sleep 9')])).toContain('>Stop<');
    expect(card([started('e1', 'sleep 9')], env({ canStop: false }))).not.toContain('>Stop<');
    expect(card([started('e1', 'sleep 9')], null)).not.toContain('>Stop<');
  });

  test('the release rules', () => {
    const running = only([started('e1', 'sleep 9')]);
    const done = only([started('e1', 'x'), completed('e1', 'done', 0)]);
    // Running on a live session, not yet 30 s without output: no release.
    expect(releaseOffered(running, env(), false)).toBe(false);
    // 30 s without output.
    expect(releaseOffered(running, env(), true)).toBe(true);
    // The session is not live.
    expect(releaseOffered(running, env({ ended: true }), false)).toBe(true);
    // Not for a member or a token without the admin scope.
    expect(releaseOffered(running, env({ ended: true, canRelease: false }), true)).toBe(false);
    expect(releaseOffered(running, undefined, true)).toBe(false);
    // Not for a settled command.
    expect(releaseOffered(done, env({ ended: true }), true)).toBe(false);
    // Not for a node that is not `shell-<id>`.
    const codex = { ...running, node_id: 'item-9' };
    expect(releaseOffered(codex, env({ ended: true }), true)).toBe(false);
    expect(card([started('e1', 'sleep 9')], env({ ended: true }))).toContain('Release the folder');
    expect(card([started('e1', 'sleep 9')])).not.toContain('Release the folder');
  });

  test('a Codex shell card of an ended session offers the release (B8 PRE-PR r1)', () => {
    const codex: Event[] = [started('E1', 'make build')];
    expect(releaseOffered(only(codex), env({ ended: true }), false)).toBe(true);
    expect(card(codex, env({ ended: true }))).toContain('Release the folder');
    expect(card(codex)).not.toContain('Release the folder');
  });

  test('the dialog asks first, with the command', () => {
    const html = renderToStaticMarkup(
      <ReleaseDialog
        command="make build"
        onRelease={async () => ({ ok: true, text: '' })}
        onClose={() => {}}
      />,
    );
    expect(html).toContain('role="dialog"');
    expect(html).toContain('aria-modal="true"');
    expect(html).toContain('Release this folder?');
    expect(html).toContain('!make build');
    expect(html).toContain(
      'is no longer running (for example after a reboot or after you checked the processes).',
    );
    expect(html).toContain('>Release<');
    expect(html).toContain('>Cancel<');
  });

  test('the chat renders the card for a shell item and a plain card for other tools', () => {
    const list = items([
      started('e1', 'pwd'),
      { ...started('x', 'ls'), node_id: 'node-2', title: 'Bash' } as Event,
    ]);
    const html = renderToStaticMarkup(
      <ChatView items={list} working={false} ready error={null} shell={env()} provider="claude" />,
    );
    expect(html).toContain('fd-chat-shell');
    expect(html.match(/fd-chat-shell /g)?.length).toBe(1);
  });
});

describe('a shell card is never folded', () => {
  const many = (): Item[] =>
    items([
      ...['a', 'b', 'c'].flatMap((id) => [
        { ...started(id, id), node_id: `node-${id}`, title: 'Read', tool_kind: 'read' } as Event,
        {
          kind: 'tool.completed',
          node_id: `node-${id}`,
          status: 'ok',
          files: [],
          exit_code: null,
        } as Event,
      ]),
      started('s1', 'pwd'),
      completed('s1', 'done', 0),
      ...['d', 'e', 'f'].flatMap((id) => [
        { ...started(id, id), node_id: `node-${id}`, title: 'Read', tool_kind: 'read' } as Event,
        {
          kind: 'tool.completed',
          node_id: `node-${id}`,
          status: 'ok',
          files: [],
          exit_code: null,
        } as Event,
      ]),
    ]);

  test('the fold splits at the shell card and the parts fold again', () => {
    const rows = unfoldShells(compactRuns(many(), false, 'claude'), false, 'claude');
    expect(rows.map((r) => r.kind)).toEqual(['fold', 'item', 'fold']);
    const middle = rows[1];
    expect(middle?.kind === 'item' && middle.item.type === 'tool' && middle.item.node_id).toBe(
      'shell-s1',
    );
  });

  test('the Commands filter still hides the card', () => {
    const hidden = applyChatFilter(
      many(),
      { ...DEFAULT_FILTER, show: { ...DEFAULT_FILTER.show, commands: false } },
      'claude',
    );
    expect(hidden.items.some((i) => i.type === 'tool' && i.node_id === 'shell-s1')).toBe(false);
  });
});

describe('the pending results (Claude)', () => {
  const ids = (events: Event[], provider: 'claude' | 'codex' = 'claude') =>
    pendingResults(items(events), provider).map((c) => c.id);

  test('the cards that completed after the latest board run', () => {
    expect(
      ids([
        started('old', 'a'),
        completed('old', 'done', 0),
        run('board'),
        started('e1', 'pwd'),
        completed('e1', 'done', 0),
        started('e2', 'ls'),
        completed('e2', 'timeout', null, false),
      ]),
    ).toEqual(['e1', 'e2']);
  });

  test('without a board run every completed card counts; a running one does not', () => {
    expect(ids([started('e1', 'a'), completed('e1', 'done', 0), started('e2', 'b')])).toEqual([
      'e1',
    ]);
    expect(ids([run('provider'), started('e1', 'a'), completed('e1', 'done', 0)])).toEqual(['e1']);
  });

  test('Codex has no chips', () => {
    expect(ids([started('e1', 'a'), completed('e1', 'done', 0)], 'codex')).toEqual([]);
  });

  test('a prompt sends the first 64 chips and the other chips stay (B8)', () => {
    const events: Event[] = [run('board')];
    for (let n = 0; n < 70; n++) events.push(started(`e${n}`, 'x'), completed(`e${n}`, 'done', 0));
    const seen: { sent: string[]; left: string[] } = { sent: [], left: [] };
    // A static render applies a state update made during the render, so the probe
    // plays one send: `toSend` names the ids, `consume` hides only those.
    function Probe() {
      const chips = useShellChips('s', items(events), 'claude');
      const [done, setDone] = useState(false);
      if (!done) {
        setDone(true);
        seen.sent = chips.toSend();
        chips.consume(seen.sent);
      }
      seen.left = chips.chips.map((c) => c.id);
      return null;
    }
    renderToStaticMarkup(<Probe />);
    expect(seen.sent).toHaveLength(64);
    expect(seen.sent[0]).toBe('e0');
    expect(seen.sent[63]).toBe('e63');
    expect(seen.left).toEqual(['e64', 'e65', 'e66', 'e67', 'e68', 'e69']);
  });

  test('a chip shows its text and a remove button', () => {
    const html = renderToStaticMarkup(
      <ShellChips chips={[{ id: 'e1', command: 'pwd' }]} onRemove={() => {}} />,
    );
    expect(html).toContain('Includes the output of');
    expect(html).toContain('!pwd');
    expect(html).toContain('Remove the output of !pwd');
    expect(renderToStaticMarkup(<ShellChips chips={[]} onRemove={() => {}} />)).toBe('');
  });
});

describe('the actions', () => {
  const posts: { path: string; body: unknown }[] = [];
  const actions = sessionActions(
    async (path, body) => {
      posts.push({ path, body });
      return { state: 'done', receipt: { state: 'accepted' } };
    },
    { station_id: 'station-a', session_id: 's1' },
    ['drive', 'admin'],
    () => 'c1',
  );

  test('shellCommand and releaseShell post their commands', async () => {
    posts.length = 0;
    await actions.shellCommand('pwd');
    await actions.releaseShell('e1');
    expect(posts).toEqual([
      {
        path: '/api/commands/session.shell_command',
        body: {
          station: 'station-a',
          command_id: 'c1',
          command: { op: 'shell_command', session_id: 's1', command: 'pwd' },
        },
      },
      {
        path: '/api/commands/session.release_shell',
        body: {
          station: 'station-a',
          command_id: 'c1',
          command: { op: 'release_shell', session_id: 's1', effect_id: 'e1' },
        },
      },
    ]);
  });

  test('send puts shell_results in the input only when there are some', async () => {
    posts.length = 0;
    await actions.send('prompt', 'go', [], ['e1', 'e2']);
    await actions.send('prompt', 'go', [], []);
    await actions.send('prompt', 'go');
    const inputs = posts.map((p) => (p.body as { command: { input: unknown } }).command.input);
    expect(inputs).toEqual([
      { text: 'go', shell_results: ['e1', 'e2'] },
      { text: 'go' },
      { text: 'go' },
    ]);
  });
});
