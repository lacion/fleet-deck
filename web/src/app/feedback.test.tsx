import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { renderToStaticMarkup } from 'react-dom/server';
import { App } from '../App';
import { FleetBoard } from '../board/FleetBoard';
import type { SpawnActions } from '../spawn/spawn';
import { markOnStart, type PlanActions } from '../board/plans';
import { liveProposer, proposerNote } from '../board/plans';
import { PlanLibrary } from '../board/PlanLibrary';
import { deckPlan } from '../board/plans.test';
import type { DeckSession } from '../gen';
import { card, station } from '../testing/fleet';
import {
  failureText,
  firstLine,
  isStartFailure,
  START_FAILURE_WINDOW_MS,
  startFailures,
} from './failures';
import { globalAction, keyAction } from './keys';
import { enterOverlay, overlayOpen } from './overlay';
import { Palette } from './Palette';

const NOW = 1_760_000_000_000;
const error = {
  code: 'start_failed',
  message: 'git clone failed\nfatal: not found',
  retry_at: null,
};
const failed = (id: string, extra: Partial<DeckSession['summary']> = {}): DeckSession =>
  card(id, 'ended', 'station-a', { last_error: error, updated_at: NOW - 1000, ...extra });

describe('start failures (R7.1)', () => {
  test('an ended session with a start_failed error is a failure; others are not', () => {
    expect(isStartFailure(failed('a'))).toBe(true);
    expect(isStartFailure(card('b', 'ended'))).toBe(false);
    expect(isStartFailure({ ...failed('c'), status: 'working' })).toBe(false);
    expect(
      isStartFailure(card('d', 'ended', 'station-a', { last_error: { ...error, code: 'x' } })),
    ).toBe(false);
  });

  test('the banner lists each failed session once, with the first line, and skips old and dismissed ones', () => {
    const sessions = [
      failed('a', { title: 'Fix the cache' }),
      failed('a'),
      failed('old', { updated_at: NOW - START_FAILURE_WINDOW_MS - 1 }),
      failed('gone'),
      card('ok', 'working'),
    ];
    const list = startFailures(sessions, NOW, new Set(['station-a/gone']));
    expect(list.map((f) => f.key.session_id)).toEqual(['a', 'a']);
    const first = list[0];
    expect(first && failureText(first)).toBe(
      'Session Fix the cache did not start: git clone failed',
    );
    expect(firstLine('one\ntwo')).toBe('one');
  });

  test('the card says "Failed to start" with the first line of the error', () => {
    const html = renderToStaticMarkup(
      <FleetBoard sessions={[failed('a')]} stations={[station()]} freshness="live" />,
    );
    expect(html).toContain('Failed to start: git clone failed');
    expect(html).not.toContain('fatal: not found');
  });

  test('the app frame shows the banner with Open and Dismiss', () => {
    const html = renderToStaticMarkup(
      <App
        sessions={[failed('a', { title: 'Fix the cache', updated_at: Date.now() })]}
        stations={[station()]}
        freshness="live"
      />,
    );
    expect(html).toContain('Session Fix the cache did not start: git clone failed');
    expect(html).toContain('>Open</button>');
    expect(html).toContain('aria-label="Dismiss: session Fix the cache did not start"');
  });
});

describe('plan execute (R7.2)', () => {
  const spawnWith = (ok: boolean): SpawnActions => ({
    canDrive: true,
    list: async () => {
      throw new Error('unused');
    },
    preflight: async () => ({ ok: false, text: 'unused' }),
    start: async () => ({ ok, text: ok ? 'Started' : 'No', sessionId: ok ? 's9' : null }),
  });
  const calls: string[] = [];
  const plans: PlanActions = {
    canDrive: true,
    list: async () => [],
    mark: async (id, status) => {
      calls.push(`${id}:${status}`);
      return deckPlan('p1', { status });
    },
  };

  test('a start that worked marks the plan executed; a failed start leaves it proposed', async () => {
    calls.length = 0;
    const errors: unknown[] = [];
    const bad = markOnStart(spawnWith(false), plans, 'plan-1', (e) => errors.push(e));
    expect((await bad.start('station-a', {} as never, 'x')).ok).toBe(false);
    expect(calls).toEqual([]);
    const good = markOnStart(spawnWith(true), plans, 'plan-1', (e) => errors.push(e));
    expect((await good.start('station-a', {} as never, 'x')).ok).toBe(true);
    await Promise.resolve();
    expect(calls).toEqual(['plan-1:executed']);
    expect(errors).toEqual([]);
  });

  test('a failed mark goes to the error callback', async () => {
    const errors: unknown[] = [];
    const broken: PlanActions = {
      ...plans,
      mark: async () => {
        throw new Error('no');
      },
    };
    const wrapped = markOnStart(spawnWith(true), broken, 'plan-1', (e) => errors.push(e));
    await wrapped.start('station-a', {} as never, 'x');
    await new Promise((r) => setTimeout(r, 0));
    expect(errors).toHaveLength(1);
  });

  test('a live proposer gets the note; an ended one does not', () => {
    const plan = deckPlan('p1');
    const live = card('s1', 'working', 'station-a', { callsign: 'Falcon' });
    expect(liveProposer(plan, [live])).toBe(live);
    expect(liveProposer(plan, [card('s1', 'ended')])).toBeUndefined();
    const render = (sessions: DeckSession[]) =>
      renderToStaticMarkup(
        <PlanLibrary
          stations={[station()]}
          sessions={sessions}
          actions={undefined}
          live
          initial={[plan]}
          onOpen={() => {}}
          onExecute={() => {}}
        />,
      );
    expect(render([live])).toContain(proposerNote('Falcon'));
    expect(render([card('s1', 'ended')])).not.toContain('is still running');
  });
});

describe('palette start row (R6.3)', () => {
  const render = (query: string, sessions: DeckSession[], onStart?: () => void) =>
    renderToStaticMarkup(
      <Palette
        sessions={sessions}
        stations={[station()]}
        onOpen={() => {}}
        onClose={() => {}}
        onStart={onStart}
        initialQuery={query}
      />,
    );

  test('no match gives the start row with the query', () => {
    const html = render(
      'fix the cache',
      [card('a', 'working', 'station-a', { title: 'Other' })],
      () => {},
    );
    expect(html).toContain('Start a session: “fix the cache”');
    expect(html).toContain('data-action="start"');
    expect(html).not.toContain('No matching sessions');
  });

  test('a match, an empty query, or no handler gives no start row', () => {
    const sessions = [card('a', 'working', 'station-a', { title: 'Fix the cache' })];
    expect(render('cache', sessions, () => {})).not.toContain('Start a session');
    expect(render('  ', [], () => {})).not.toContain('Start a session');
    expect(render('zzz', [], undefined)).toContain('No matching sessions');
  });
});

describe('overlays block the board hotkeys (R7.4)', () => {
  test('a set flag turns off every key except Mod+K and Escape', () => {
    expect(globalAction({ key: 'j' }, false)).toBe('next');
    for (const key of ['j', 'k', '/', 'n', 'f', 'c', 't', '?']) {
      expect(globalAction({ key }, true)).toBeNull();
    }
    expect(globalAction({ key: 'Escape' }, true)).toBe('escape');
    expect(globalAction({ key: 'k', ctrlKey: true }, true)).toBe('palette');
    expect(keyAction({ key: 'j' })).toBe('next');
  });

  test('the flag counts overlays and each release is used once', () => {
    expect(overlayOpen()).toBe(false);
    const a = enterOverlay();
    const b = enterOverlay();
    a();
    a();
    expect(overlayOpen()).toBe(true);
    b();
    expect(overlayOpen()).toBe(false);
  });

  test('every component with a modal dialog calls useOverlay', () => {
    const files: string[] = [];
    const walk = (dir: string) => {
      for (const name of readdirSync(dir)) {
        const path = join(dir, name);
        if (statSync(path).isDirectory()) walk(path);
        else if (path.endsWith('.tsx') && !path.endsWith('.test.tsx')) files.push(path);
      }
    };
    walk(join(import.meta.dir, '..'));
    const modal = /\saria-modal="true"/g;
    const missing = files.flatMap((path) => {
      const source = readFileSync(path, 'utf8');
      const dialogs = source.match(modal)?.length ?? 0;
      const hooks = source.match(/\buseOverlay\(\)/g)?.length ?? 0;
      return dialogs > hooks ? [`${path}: ${dialogs} dialogs, ${hooks} useOverlay`] : [];
    });
    expect(missing).toEqual([]);
    // The scan must find the known overlays, or it checks nothing.
    const found = files
      .filter((p) => /\saria-modal="true"/.test(readFileSync(p, 'utf8')))
      .map((p) => p.split('/').pop());
    expect(found.sort()).toEqual(
      [
        'DeviceLinkDialog.tsx',
        'HelpOverlay.tsx',
        'Layout.tsx',
        'Palette.tsx',
        'PairingDialog.tsx',
        'PublishDialog.tsx',
        'ShareDialog.tsx',
      ].sort(),
    );
  });
});
