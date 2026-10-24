import { describe, expect, test } from 'bun:test';
import type { NativePreview, NativeSession, NativeSessionList, StationProvider } from '../gen';
import { ApiError } from '../runtime/api';
import {
  ageText,
  continueFill,
  entryTitle,
  groupNative,
  homeLine,
  importStateLine,
  isNativeList,
  isNativePreview,
  listNotes,
  nativeActions,
  nativeError,
  previewQuery,
  unverifiedNote,
} from './nativeSessions';

const provider = (instance: string, kind: 'claude' | 'codex' = 'claude'): StationProvider => ({
  instance,
  provider: kind,
  version: null,
  auth: 'signed_in',
  version_status: 'unknown',
  capabilities: null,
});
const PROVIDERS = [provider('claude'), provider('codex', 'codex')];
const PIN = { home: '/home/me/.claude', dev: 7, ino: 99 };

const native = (id: string, extra: Partial<NativeSession> = {}): NativeSession => ({
  provider: 'claude',
  native_id: id,
  instances: ['claude'],
  pin: PIN,
  cwd: '/home/me/app',
  title: null,
  first_prompt: null,
  modified_at: 1_000_000,
  size: 2048,
  parent_native_id: null,
  known: null,
  import: { state: 'ok' },
  verified: true,
  ...extra,
});
const listOf = (
  sessions: NativeSession[],
  extra: Partial<NativeSessionList> = {},
): NativeSessionList => ({
  homes: [],
  sessions,
  truncated: false,
  timed_out: false,
  ...extra,
});

describe('the import state texts (B7)', () => {
  test('every state has its exact text', () => {
    const text = (state: NativeSession['import']) => importStateLine(state)?.text;
    expect(importStateLine({ state: 'ok' })).toBeNull();
    expect(text({ state: 'outside_roots' })).toBe(
      "The folder is outside this station's allowed folders.",
    );
    expect(text({ state: 'in_use', session_id: 's1' })).toBe('A Fleet Deck session uses it.');
    expect(text({ state: 'held', session_id: 's1' })).toBe('A Fleet Deck session holds it.');
    expect(text({ state: 'execution_unknown' })).toBe(
      'It may still run elsewhere. Close it there first.',
    );
    expect(text({ state: 'cwd_mismatch' })).toBe(
      'Its folder path changed (a link?). Open it from its real folder.',
    );
    expect(text({ state: 'too_large' })).toBe('The transcript is too large to import.');
    expect(text({ state: 'subagent_thread' })).toBe('A subagent thread. Continue its parent.');
    expect(text({ state: 'path_not_trusted' })).toBe('Another user can change this folder.');
    expect(text({ state: 'not_trusted' })).toBe('Fleet Deck cannot trust this transcript.');
  });

  test('in_use and held link to the session; the others do not', () => {
    expect(importStateLine({ state: 'in_use', session_id: 's1' })?.link).toBe('s1');
    expect(importStateLine({ state: 'held', session_id: 's2' })?.link).toBe('s2');
    expect(importStateLine({ state: 'too_large' })?.link).toBeNull();
  });

  test('an importable entry that is not verified gets the note; a verified one does not', () => {
    expect(unverifiedNote(native('a', { verified: false }))).toBe(
      'Fleet Deck cannot verify this folder.',
    );
    expect(unverifiedNote(native('a'))).toBeNull();
    expect(
      unverifiedNote(native('a', { verified: false, import: { state: 'too_large' } })),
    ).toBeNull();
  });
});

describe('the list view model (B7)', () => {
  test('groups by instance in the order of the providers, with the heading', () => {
    const groups = groupNative(
      listOf([
        native('c1', { instances: ['codex'], provider: 'codex' }),
        native('a1'),
        native('a2'),
      ]),
      PROVIDERS,
    );
    expect(groups.map((g) => g.heading)).toEqual(['claude · Claude', 'codex · Codex']);
    expect(groups[0]?.rows.map((r) => r.session.native_id)).toEqual(['a1', 'a2']);
  });

  test('a child thread follows its listed parent, indented', () => {
    const rows = groupNative(
      listOf([
        native('p1', { instances: ['codex'], provider: 'codex' }),
        native('x', { instances: ['codex'], provider: 'codex' }),
        native('c1', { instances: ['codex'], provider: 'codex', parent_native_id: 'p1' }),
      ]),
      PROVIDERS,
    )[0]?.rows.map((r) => [r.session.native_id, r.depth]);
    expect(rows).toEqual([
      ['p1', 0],
      ['c1', 1],
      ['x', 0],
    ]);
  });

  test('a child thread whose parent is not listed stays in place', () => {
    const rows = groupNative(
      listOf([native('a'), native('c1', { parent_native_id: 'gone' }), native('b')]),
      PROVIDERS,
    )[0]?.rows.map((r) => [r.session.native_id, r.depth]);
    expect(rows).toEqual([
      ['a', 0],
      ['c1', 1],
      ['b', 0],
    ]);
  });

  test('a home that is not listed gives one muted line with its reason', () => {
    const list = listOf([], {
      homes: [
        {
          provider: 'claude',
          instances: ['claude', 'work'],
          state: { state: 'unavailable', reason: 'home_not_absolute' },
        },
      ],
    });
    expect(homeLine(list.homes[0] as NonNullable<(typeof list.homes)[0]>)).toBe(
      'claude, work: the home folder is not an absolute path.',
    );
    const g = groupNative(list, PROVIDERS);
    expect(g[0]?.unavailable).toEqual(['claude, work: the home folder is not an absolute path.']);
    const reason = (r: string) =>
      homeLine({
        provider: 'claude',
        instances: ['claude'],
        state: { state: 'unavailable', reason: r },
      });
    expect(reason('not_readable')).toBe('claude: the home folder cannot be read.');
    expect(reason('home_not_trusted')).toBe('claude: another user can change the home folder.');
    expect(reason('odd')).toBe('claude: odd');
    expect(
      homeLine({ provider: 'claude', instances: ['claude'], state: { state: 'ok' } }),
    ).toBeNull();
  });

  test('the notes of a cut list and of a stopped list', () => {
    expect(listNotes(listOf([native('a'), native('b')], { truncated: true }))).toEqual([
      'Showing the newest 2 sessions.',
    ]);
    expect(listNotes(listOf([], { timed_out: true }))).toEqual([
      'The list stopped early; it may be incomplete.',
    ]);
    expect(listNotes(listOf([]))).toEqual([]);
  });

  test('the title, age and size texts', () => {
    expect(entryTitle(native('a', { title: ' Fix it ' }))).toBe('Fix it');
    expect(entryTitle(native('a', { first_prompt: 'one\n  two' }))).toBe('one two');
    expect(entryTitle(native('abc'))).toBe('abc');
    expect(ageText(1_000_000, 1_000_000 + 5 * 60_000)).toBe('5 min ago');
    expect(ageText(1_000_000, 1_000_000 + 2 * 86_400_000)).toBe('2 d ago');
    expect(ageText(1_000_000, 1_000_000 + 1000)).toBe('now');
  });
});

describe('Continue it (B7)', () => {
  test('an importable entry fills the instance, the folder, the id and keeps the pin', () => {
    expect(continueFill(native('abc', { cwd: '/w/app' }), PROVIDERS)).toEqual({
      instance: 'claude',
      cwd: '/w/app',
      importId: 'abc',
      pin: PIN,
    });
  });

  test('an unverified entry continues without a pin', () => {
    expect(continueFill(native('abc', { verified: false }), PROVIDERS)?.pin).toBeNull();
  });

  test('the instance is the first listed one that the station has', () => {
    const s = native('abc', { instances: ['other', 'codex'] });
    expect(continueFill(s, PROVIDERS)?.instance).toBe('codex');
    expect(previewQuery(s, 'codex')).toEqual({
      provider_instance: 'codex',
      native_id: 'abc',
      pin: PIN,
    });
  });

  test('any state but ok, a missing folder, or an unknown instance cannot continue', () => {
    expect(continueFill(native('a', { import: { state: 'too_large' } }), PROVIDERS)).toBeNull();
    expect(
      continueFill(native('a', { import: { state: 'in_use', session_id: 's' } }), PROVIDERS),
    ).toBeNull();
    expect(continueFill(native('a', { cwd: null }), PROVIDERS)).toBeNull();
    expect(continueFill(native('a', { instances: ['nope'] }), PROVIDERS)).toBeNull();
  });
});

describe('the fetch helpers (B7)', () => {
  const list = listOf([native('a')]);
  const preview: NativePreview = {
    items: [],
    state: { kind: 'complete' },
    cwd: '/w',
    size: 1,
    modified_at: 1,
    earlier_omitted: false,
  };

  test('the list is a GET of the station route; the preview a POST of the query', async () => {
    const calls: unknown[][] = [];
    const actions = nativeActions(
      async (path) => {
        calls.push(['get', path]);
        return list;
      },
      async (path, body) => {
        calls.push(['post', path, body]);
        return preview;
      },
    );
    expect(await actions.list('st 1')).toEqual(list);
    const q = previewQuery(native('a'), 'claude');
    expect(await actions.preview('st 1', q)).toEqual(preview);
    expect(calls).toEqual([
      ['get', '/api/stations/st%201/native-sessions'],
      ['post', '/api/stations/st%201/native-sessions/preview', q],
    ]);
  });

  test('an answer of another shape is refused', async () => {
    const actions = nativeActions(
      async () => ({ homes: 1 }),
      async () => ({ items: 1 }),
    );
    await expect(actions.list('s')).rejects.toThrow('cannot read');
    await expect(actions.preview('s', previewQuery(native('a'), 'claude'))).rejects.toThrow(
      'cannot read',
    );
    expect(isNativeList(list)).toBe(true);
    expect(isNativePreview(preview)).toBe(true);
    expect(isNativePreview(null)).toBe(false);
  });

  test('errors: the deck text, a retry for all but the 409', () => {
    const busy = new ApiError(503, 'The station is busy. Try again in a moment.', 'busy');
    expect(nativeError(busy)).toEqual({
      text: 'The station is busy. Try again in a moment.',
      retry: true,
    });
    const old = new ApiError(
      409,
      'This station does not list its sessions yet. Update it.',
      'native_sessions_unsupported',
    );
    expect(nativeError(old)).toEqual({
      text: 'This station does not list its sessions yet. Update it.',
      retry: false,
    });
    expect(nativeError(new Error('boom'))).toEqual({ text: 'boom', retry: true });
    expect(nativeError('x')).toEqual({ text: 'The request failed.', retry: true });
  });
});
