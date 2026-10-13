import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { join, JOIN_ERRORS, joinErrorText, joinToken, displayName } from './join';
import { JoinPage } from './JoinPage';
import { MEMBER_KEY, memberOf, readStoredMember, writeStoredMember } from './member';
import type { AuthView } from '../runtime/state';

const page = (extra: Partial<Parameters<typeof JoinPage>[0]> = {}) =>
  renderToStaticMarkup(
    <JoinPage onJoin={async () => ({ ok: true, joined: null })} onJoined={() => {}} {...extra} />,
  );

describe('join page (R11.4)', () => {
  test('the title, the name field, the button and the note', () => {
    const html = page();
    expect(html).toContain('Join a shared Fleet Deck');
    expect(html).toContain('Display name');
    expect(html).toContain('maxLength="40"');
    expect(html).toContain('>Join</button>');
    expect(html).toContain(
      'You join as a guest of the owner&#x27;s deck. The owner can end your access at any time.',
    );
    expect(html).not.toContain('role="alert"');
    // An empty name cannot be sent.
    expect(html).toMatch(/<button type="submit" disabled="">Join/);
  });

  test('a valid name enables the button; a busy page shows Joining', () => {
    expect(page({ initialName: 'Ana' })).not.toMatch(/<button type="submit" disabled/);
    expect(page({ initialName: 'Ana', initialBusy: true })).toContain('Joining…');
  });

  test('an error is an alert', () => {
    const html = page({ initialError: JOIN_ERRORS.invite });
    expect(html).toContain('role="alert"');
    expect(html).toContain('This invite link is not valid any more. Ask for a new one.');
  });

  test('the token of a location hash', () => {
    expect(joinToken('#join=abc123')).toBe('abc123');
    expect(joinToken('#join=a%20b')).toBe('a b');
    expect(joinToken('#join=')).toBeNull();
    expect(joinToken('#/s/a/b')).toBeNull();
    expect(joinToken('')).toBeNull();
    expect(joinToken('#join=%E0%A4%A')).toBeNull();
  });

  test('the display name has 1 to 40 characters', () => {
    expect(displayName('  Ana  ')).toBe('Ana');
    expect(displayName('   ')).toBeNull();
    expect(displayName('x'.repeat(40))).toBe('x'.repeat(40));
    expect(displayName('x'.repeat(41))).toBeNull();
  });
});

const reply = (status: number, body: unknown) => async (): Promise<Response> =>
  new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });

describe('the join call (R11.4)', () => {
  test('it posts the token and the name, and returns the member answer', async () => {
    const calls: { path: string; init: RequestInit }[] = [];
    const result = await join(
      async (path, init) => {
        calls.push({ path, init });
        return new Response(
          JSON.stringify({ actor: 'member:Ana', role: 'collaborator', display_name: 'Ana' }),
          { status: 200 },
        );
      },
      'tok',
      'Ana',
    );
    expect(result).toEqual({ ok: true, joined: { role: 'collaborator', display_name: 'Ana' } });
    const seen = calls[0];
    expect(seen?.path).toBe('/api/auth/join');
    expect(seen?.init.method).toBe('POST');
    expect(JSON.parse(String(seen?.init.body))).toEqual({ token: 'tok', display_name: 'Ana' });
    expect(seen?.init.credentials).toBe('same-origin');
  });

  test('an answer without the member fields still joins', async () => {
    expect(await join(reply(200, { actor: 'member:Ana' }), 't', 'Ana')).toEqual({
      ok: true,
      joined: null,
    });
  });

  test('401 invalid_invite, 429 and 400 invalid_display_name have their texts', async () => {
    const run = (status: number, error: string) =>
      join(reply(status, { error, message: 'deck text' }), 't', 'Ana');
    expect(await run(401, 'invalid_invite')).toEqual({ ok: false, text: JOIN_ERRORS.invite });
    expect(await run(429, 'too_many_attempts')).toEqual({
      ok: false,
      text: 'Too many tries. Wait a minute.',
    });
    expect(await run(400, 'invalid_display_name')).toEqual({
      ok: false,
      text: 'Use 1 to 40 letters, digits or spaces.',
    });
  });

  test('another error shows the message of the deck; no answer shows a network text', async () => {
    expect(
      await join(reply(500, { error: 'internal', message: 'the deck failed' }), 't', 'A'),
    ).toEqual({
      ok: false,
      text: 'the deck failed',
    });
    expect(joinErrorText(502, null, null)).toBe('The deck refused the request (502).');
    const failed = await join(
      async () => {
        throw new Error('offline');
      },
      't',
      'A',
    );
    expect(failed.ok).toBe(false);
  });
});

const login = (extra: Partial<AuthView> = {}): AuthView => ({
  actor: 'member:Ana',
  scopes: ['read'],
  csrf: 'c',
  expires_at: 1,
  ...extra,
});

describe('the member of a login (R11.4)', () => {
  const store = (value: string | null) => ({ getItem: () => value });

  test('the login answer comes first', () => {
    expect(memberOf(login({ role: 'approver', display_name: 'Ana B' }), store(null))).toEqual({
      role: 'approver',
      name: 'Ana B',
      scope: null,
    });
  });

  test('without a role the stored join answer is the fallback', () => {
    const stored = store(JSON.stringify({ role: 'viewer', display_name: 'Ana' }));
    expect(memberOf(login(), stored)).toEqual({ role: 'viewer', name: 'Ana', scope: null });
    expect(memberOf(login(), store(null))).toBeNull();
    expect(memberOf(login(), store('not json'))).toBeNull();
    expect(
      memberOf(login(), store(JSON.stringify({ role: 'root', display_name: 'A' }))),
    ).toBeNull();
  });

  test('the owner and a token are no members', () => {
    const stored = store(JSON.stringify({ role: 'viewer', display_name: 'Ana' }));
    expect(memberOf(login({ actor: 'owner' }), stored)).toBeNull();
    expect(memberOf(login({ actor: 'token:ci' }), stored)).toBeNull();
    expect(memberOf(null, stored)).toBeNull();
  });

  test('blocked storage never throws', () => {
    const blocked = {
      getItem: () => {
        throw new Error('blocked');
      },
      setItem: () => {
        throw new Error('blocked');
      },
      removeItem: () => {
        throw new Error('blocked');
      },
    };
    expect(readStoredMember(blocked)).toBeNull();
    expect(() => writeStoredMember(blocked, { role: 'viewer', display_name: 'A' })).not.toThrow();
    expect(() => writeStoredMember(blocked, null)).not.toThrow();
    expect(readStoredMember(null)).toBeNull();
  });

  test('the answer is written under the fd-member key', () => {
    const data = new Map<string, string>();
    const storage = {
      getItem: (k: string) => data.get(k) ?? null,
      setItem: (k: string, v: string) => void data.set(k, v),
      removeItem: (k: string) => void data.delete(k),
    };
    writeStoredMember(storage, { role: 'collaborator', display_name: 'Bo' });
    expect(MEMBER_KEY).toBe('fd-member');
    expect(readStoredMember(storage)).toEqual({ role: 'collaborator', name: 'Bo', scope: null });
    writeStoredMember(storage, null);
    expect(readStoredMember(storage)).toBeNull();
  });
});
