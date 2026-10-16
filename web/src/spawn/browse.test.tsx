import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { NativePreview, NativeSession, NativeSessionList, StationProvider } from '../gen';
import { BrowseSessions } from './BrowseSessions';
import type { NativeActions } from './nativeSessions';

const provider = (instance: string, kind: 'claude' | 'codex'): StationProvider => ({
  instance,
  provider: kind,
  version: null,
  auth: 'signed_in',
  version_status: 'unknown',
  capabilities: null,
});
const PROVIDERS = [provider('claude', 'claude'), provider('codex', 'codex')];
const native = (id: string, extra: Partial<NativeSession> = {}): NativeSession => ({
  provider: 'claude',
  native_id: id,
  instances: ['claude'],
  pin: { home: '/home/me/.claude', dev: 7, ino: 99 },
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
const NOW = 1_000_000 + 5 * 60_000;
const never: NativeActions = {
  list: () => new Promise(() => undefined),
  preview: () => new Promise(() => undefined),
};

const list = (
  sessions: NativeSession[],
  extra: Partial<NativeSessionList> = {},
): NativeSessionList => ({ homes: [], sessions, truncated: false, timed_out: false, ...extra });

const dialog = (
  props: {
    initialList?: NativeSessionList;
    initialChosen?: NativeSession;
    initialPreview?: NativePreview;
  } = {},
) =>
  renderToStaticMarkup(
    <BrowseSessions
      stationId="st-1"
      stationName="Workstation"
      providers={PROVIDERS}
      actions={never}
      onClose={() => undefined}
      onContinue={() => undefined}
      now={NOW}
      {...props}
    />,
  );

const preview = (extra: Partial<NativePreview> = {}): NativePreview => ({
  items: [
    { id: 'i1', at: 0, kind: 'user', text: 'Fix the bug', tool: null },
    { id: 'i2', at: 0, kind: 'assistant', text: 'Done.', tool: null },
  ],
  state: { kind: 'complete' },
  cwd: '/home/me/app',
  size: 2048,
  modified_at: 1_000_000,
  earlier_omitted: false,
  ...extra,
});

describe('the sessions dialog (B7)', () => {
  test('opens as a modal dialog named after the station, loading first', () => {
    const html = dialog();
    expect(html).toContain('role="dialog"');
    expect(html).toContain('aria-modal="true"');
    expect(html).toContain('aria-label="Sessions on Workstation"');
    expect(html).toContain('<h2>Sessions on Workstation</h2>');
    expect(html).toContain('Reading the sessions…');
    expect(html).toContain('Close');
  });

  test('an empty list says so', () => {
    expect(dialog({ initialList: list([]) })).toContain('No sessions on this station.');
  });

  test('a row shows the title, folder, age, size and opens the preview as a button', () => {
    const html = dialog({
      initialList: list([native('a1', { title: 'Fix the checks', cwd: '/home/me/app' })]),
    });
    expect(html).toContain('claude · Claude');
    expect(html).toContain('Fix the checks');
    expect(html).toContain('/home/me/app');
    expect(html).toContain('5m ago · 2.0 KB');
    expect(html).toContain('class="fd-native-open"');
    expect(html).not.toContain('Already in Fleet Deck');
    expect(html).not.toContain('Fleet Deck cannot verify');
  });

  test('a known session shows "Already in Fleet Deck" with a link to it', () => {
    const html = dialog({
      initialList: list([
        native('a1', { known: { session_id: 's9', state: 'running' }, import: { state: 'ok' } }),
      ]),
    });
    expect(html).toContain('Already in Fleet Deck');
    expect(html).toContain('href="#/s/st-1/s9"');
    expect(html).toContain('Open it');
  });

  test('in_use shows its text with a link, once', () => {
    const html = dialog({
      initialList: list([
        native('a1', {
          known: { session_id: 's9', state: 'running' },
          import: { state: 'in_use', session_id: 's9' },
        }),
      ]),
    });
    expect(html).toContain('A Fleet Deck session uses it.');
    expect(html.match(/Open it/g)?.length).toBe(1);
  });

  test('an unverified importable entry shows the note; every other state its text', () => {
    expect(dialog({ initialList: list([native('a', { verified: false })]) })).toContain(
      'Fleet Deck cannot verify this folder.',
    );
    const html = dialog({
      initialList: list([
        native('b', { import: { state: 'path_not_trusted' } }),
        native('c', { import: { state: 'not_trusted' } }),
      ]),
    });
    expect(html).toContain('Another user can change this folder.');
    expect(html).toContain('Fleet Deck cannot trust this transcript.');
  });

  test('child threads sit under their parent, indented', () => {
    const html = dialog({
      initialList: list([
        native('p1', { instances: ['codex'], provider: 'codex', title: 'Parent' }),
        native('c1', {
          instances: ['codex'],
          provider: 'codex',
          title: 'Child',
          parent_native_id: 'p1',
          import: { state: 'subagent_thread' },
        }),
      ]),
    });
    expect(html.indexOf('Parent')).toBeLessThan(html.indexOf('Child'));
    expect(html).toContain('data-depth="1"');
    expect(html).toContain('A subagent thread. Continue its parent.');
  });

  test('a home that is not listed, and the cut and stopped notes', () => {
    const html = dialog({
      initialList: list([native('a')], {
        truncated: true,
        timed_out: true,
        homes: [
          {
            provider: 'codex',
            instances: ['codex'],
            state: { state: 'unavailable', reason: 'not_readable' },
          },
        ],
      }),
    });
    expect(html).toContain('codex: the home folder cannot be read.');
    expect(html).toContain('Showing the newest 1 sessions.');
    expect(html).toContain('The list stopped early; it may be incomplete.');
  });

  test('the preview shows the items, the omitted line and an enabled Continue it', () => {
    const html = dialog({
      initialList: list([native('a1')]),
      initialChosen: native('a1', { title: 'Fix the checks' }),
      initialPreview: preview({ earlier_omitted: true }),
    });
    expect(html).toContain('Back to the list');
    expect(html).toContain('Fix the bug');
    expect(html).toContain('Done.');
    expect(html).toContain('Earlier messages are not shown.');
    expect(html).toMatch(/<button[^>]*class="fd-primary"[^>]*>Continue it/);
    expect(html).not.toMatch(/<button[^>]*disabled=""[^>]*>Continue it/);
  });

  test('Continue it is off, with the reason, for a state other than ok', () => {
    const html = dialog({
      initialChosen: native('a1', { import: { state: 'execution_unknown' } }),
      initialPreview: preview(),
    });
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>Continue it/);
    expect(html).toContain('It may still run elsewhere. Close it there first.');
  });

  test('an unavailable preview shows the history line and no omitted line', () => {
    const html = dialog({
      initialChosen: native('a1'),
      initialPreview: preview({
        items: [],
        state: { kind: 'unavailable', reason: 'not_trusted' },
      }),
    });
    expect(html).toContain('the transcript file failed a safety check.');
    expect(html).not.toContain('Earlier messages are not shown.');
  });
});
