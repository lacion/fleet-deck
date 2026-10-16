// B8: the `!` mode of the composer, who gets it, and the chips on the session view.
import { afterEach, describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DeckSession, Event, StationView } from '../gen';
import type { Timeline } from '../runtime/state';
import { memberEnv } from '../sharing/fixtures';
import { ownerEnv, PeopleProvider } from '../sharing/context';
import { station } from '../testing/fleet';
import { summary } from '../testing/summary';
import { claudeCapabilities, envelopes } from '../testing/timeline';
import { sessionActions } from './commands';
import { Composer, inputOf } from './Composer';
import { SessionView } from './SessionView';

const DRAFT = 'fd-draft:station-a/s1';
const drafts = (value: string | null) => {
  const store: Record<string, string> = value === null ? {} : { [DRAFT]: value };
  (globalThis as { sessionStorage?: unknown }).sessionStorage = {
    getItem: (k: string) => store[k] ?? null,
    setItem: (k: string, v: string) => {
      store[k] = v;
    },
    removeItem: (k: string) => {
      delete store[k];
    },
  };
};
afterEach(() => {
  (globalThis as { sessionStorage?: unknown }).sessionStorage = undefined;
});

const composer = (draft: string, extra: Partial<Parameters<typeof Composer>[0]> = {}) => {
  drafts(draft);
  return renderToStaticMarkup(
    <Composer
      enabled
      working={false}
      steer={false}
      nativeQueue={false}
      onSend={() => {}}
      draftKey="station-a/s1"
      bang
      {...extra}
    />,
  );
};

describe('what the text means (B8)', () => {
  test('a text that starts with ! is a shell command', () => {
    expect(inputOf('!pwd', true)).toEqual({ kind: 'shell', command: 'pwd' });
    expect(inputOf('  !  ls -la  ', true)).toEqual({ kind: 'shell', command: 'ls -la' });
    expect(inputOf('!', true)).toEqual({ kind: 'shell', command: '' });
    expect(inputOf('!   ', true)).toEqual({ kind: 'shell', command: '' });
  });

  test('the backslash escape is a literal prompt without the backslash', () => {
    expect(inputOf('\\!not a command', true)).toEqual({ kind: 'prompt', text: '!not a command' });
    expect(inputOf('hello !x', true)).toEqual({ kind: 'prompt', text: 'hello !x' });
  });

  test('without the mode the text is always a prompt', () => {
    expect(inputOf('!pwd', false)).toEqual({ kind: 'prompt', text: '!pwd' });
    expect(inputOf('\\!pwd', false)).toEqual({ kind: 'prompt', text: '\\!pwd' });
  });
});

describe('the composer in ! mode', () => {
  test('shows the chip, the Run button and the placeholder; Send is gone', () => {
    const html = composer('!pwd');
    expect(html).toContain('aria-label="Shell command"');
    expect(html).toContain('placeholder="Shell command (runs in the session folder)"');
    expect(html).toContain('Run');
    expect(html).not.toContain('Send');
    expect(html).not.toContain('Wait for the turn to end.');
    expect(html).not.toContain('Members of this session');
  });

  test('an empty command cannot run', () => {
    expect(composer('!')).toMatch(/<button type="submit" class="fd-primary" disabled="">/);
    expect(composer('!pwd')).not.toMatch(/<button type="submit" class="fd-primary" disabled="">/);
  });

  test('while the session works: the line, and no way to send', () => {
    const html = composer('!pwd', { working: true, steer: true });
    expect(html).toContain('Wait for the turn to end.');
    expect(html).toMatch(/<button type="submit" class="fd-primary" disabled="">/);
    expect(html).not.toContain('Queue follow-up');
    expect(html).not.toContain('Steer now');
  });

  test('in a shared session: the members line', () => {
    expect(composer('!pwd', { shared: true })).toContain(
      'Members of this session see the command and its output.',
    );
  });

  test('an ordinary text and the escape stay a prompt', () => {
    for (const draft of ['hello', '\\!pwd']) {
      const html = composer(draft);
      expect(html).not.toContain('aria-label="Shell command"');
      expect(html).toContain('Send');
    }
  });

  test('without the mode a ! text is an ordinary prompt', () => {
    const html = composer('!pwd', { bang: false });
    expect(html).not.toContain('aria-label="Shell command"');
    expect(html).toContain('Send');
  });

  test('the chips show above the input', () => {
    const html = composer('', { chips: [{ id: 'e1', command: 'pwd' }] });
    expect(html).toContain('Includes the output of');
    expect(html.indexOf('Includes the output of')).toBeLessThan(html.indexOf('<textarea'));
  });
});

const card = (provider: 'claude' | 'codex' | 'unknown' = 'claude'): DeckSession => ({
  station_id: 'station-a',
  status: 'idle',
  summary: summary('s1', 'idle', 1, {
    provider,
    title: 'Shell',
    capabilities: claudeCapabilities(),
    mode: 'default',
  }),
});

const shellEvents = (): Event[] => [
  { kind: 'run.started', run_id: 'r1', input_summary: 'hi', origin: 'board' },
  {
    kind: 'tool.started',
    node_id: 'shell-e1',
    parent_node_id: null,
    tool_kind: 'shell',
    title: 'Shell command',
    input_summary: 'make test',
    files: [],
    provider_refs: {},
  },
  {
    kind: 'tool.completed',
    node_id: 'shell-e1',
    status: 'ok',
    files: [],
    exit_code: 0,
    provider_refs: { shell: 'done' },
  },
];

const timeline = (events: Event[]): Timeline => ({
  incarnation: 'inc',
  cursor: 0,
  events: envelopes('s1', events),
  freshness: 'live',
  error: null,
  ready: true,
});

const pane = (
  session: DeckSession,
  options: {
    shells?: boolean | null;
    member?: boolean;
    shared?: boolean;
    events?: Event[];
    scopes?: string[];
  } = {},
) => {
  const info = options.shells === undefined ? null : { allow_shell: options.shells };
  const stations: StationView[] = [station('station-a', { info: info as StationView['info'] })];
  return renderToStaticMarkup(
    <PeopleProvider
      value={
        options.member
          ? memberEnv('collaborator')
          : options.shared
            ? { ...ownerEnv, shared: new Set(['["station-a","s1"]']) }
            : ownerEnv
      }
    >
      <SessionView
        session={session}
        stations={stations}
        freshness="live"
        timeline={timeline(options.events ?? [])}
        actions={sessionActions(
          async () => ({}),
          { station_id: 'station-a', session_id: 's1' },
          options.scopes ?? ['read', 'drive', 'approve', 'admin'],
        )}
        layout="phone"
        tab="chat"
      />
    </PeopleProvider>,
  );
};

describe('who gets the ! mode on the session view', () => {
  const on = (html: string) => html.includes('aria-label="Shell command"');

  test('the owner with shells on or unreported, for Claude and Codex', () => {
    drafts('!pwd');
    expect(on(pane(card('claude'), { shells: true }))).toBe(true);
    expect(on(pane(card('claude')))).toBe(true);
    expect(on(pane(card('codex'), { shells: true }))).toBe(true);
  });

  test('the shared line shows for a session that a share covers', () => {
    drafts('!pwd');
    const line = 'Members of this session see the command and its output.';
    expect(pane(card('claude'), { shared: true })).toContain(line);
    expect(pane(card('claude'))).not.toContain(line);
  });

  test('not with shells off, for a member, or for an unknown provider', () => {
    drafts('!pwd');
    expect(on(pane(card('claude'), { shells: false }))).toBe(false);
    expect(on(pane(card('claude'), { shells: true, member: true }))).toBe(false);
    expect(on(pane(card('unknown'), { shells: true }))).toBe(false);
  });
});

describe('the chips on the session view', () => {
  test('a Claude session shows a chip for a finished command; Codex does not', () => {
    drafts(null);
    const claude = pane(card('claude'), { events: shellEvents() });
    expect(claude).toContain('Includes the output of');
    expect(claude).toContain('!make test');
    expect(pane(card('codex'), { events: shellEvents() })).not.toContain('Includes the output of');
  });

  test('a prompt after the card uses it up: the next board run starts after it', () => {
    drafts(null);
    const events = [
      ...shellEvents(),
      { kind: 'run.started', run_id: 'r2', input_summary: 'next', origin: 'board' } as Event,
    ];
    expect(pane(card('claude'), { events })).not.toContain('Includes the output of');
  });
});
