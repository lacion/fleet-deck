import { expect, test } from 'bun:test';
import { ApiError, authView, request, takeLoginLink } from './api';
import { loginView } from './testing';

test('one-time login secrets leave history before the link is redeemed', () => {
  const calls: unknown[][] = [];
  const token = takeLoginLink(
    { hash: '#login=fdsecret', pathname: '/board', search: '?view=fleet' },
    {
      replaceState: (...args) => {
        calls.push(args);
      },
    },
  );
  expect(token).toBe('fdsecret');
  expect(calls).toEqual([[null, '', '/board?view=fleet']]);
  expect(
    takeLoginLink(
      { hash: '#session=one', pathname: '/', search: '' },
      {
        replaceState: () => {
          throw new Error('must not strip session navigation');
        },
      },
    ),
  ).toBeNull();
});

test('login shapes and scopes are validated before a socket opens', () => {
  expect(authView(loginView).csrf).toBe('test-csrf');
  for (const bad of [null, {}, { ...loginView, csrf: 1 }, { ...loginView, scopes: ['spawn'] }]) {
    expect(() => authView(bad)).toThrow('Invalid login response');
  }
});

test('the deck version is kept when it is a string and dropped otherwise', () => {
  expect(authView({ ...loginView, deck_version: '0.9.1' }).deck_version).toBe('0.9.1');
  expect(authView(loginView)).not.toHaveProperty('deck_version');
  const odd = authView({ ...loginView, deck_version: 9 });
  expect(odd).not.toHaveProperty('deck_version');
  expect(odd.actor).toBe('owner');
});

test('REST errors keep status and message and successful logout needs no JSON body', async () => {
  const signal = new AbortController().signal;
  expect(
    await request(async () => new Response(null, { status: 204 }), '/logout', signal, {}),
  ).toBeNull();
  try {
    await request(
      async () => Response.json({ message: 'Try later' }, { status: 429 }),
      '/login',
      signal,
      {},
    );
    throw new Error('expected refusal');
  } catch (error) {
    expect(error).toBeInstanceOf(ApiError);
    expect(error instanceof ApiError && error.status).toBe(429);
    expect(error instanceof Error && error.message).toBe('Try later');
  }
});
