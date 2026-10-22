import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { RateMeter } from '../board/usage';
import type { StationInfo, StationProvider } from '../gen';
import { station } from '../testing/fleet';
import { submitKeyHint } from './keyHint';
import { AUTO, limitBadge, providerChoice } from './spawn';
import { SheetHead, SpawnFoot, StationPills } from './SpawnSheet';

const info = (extra: Partial<StationInfo>): StationInfo =>
  ({ max_sessions: 4, live_sessions: 1, ...extra }) as StationInfo;

const pills = (list: ReturnType<typeof station>[], value: string, auto = false) =>
  renderToStaticMarkup(
    <StationPills targets={list} value={value} auto={auto} onChange={() => {}} />,
  );

describe('the New session sheet (DESIGN-SYSTEM §4.6)', () => {
  test('each station is a radio pill with its name, its reach and the free count', () => {
    const html = pills([station('a', { info: info({}) }), station('b', { info: info({}) })], 'a');
    expect(html).toContain('role="radiogroup" aria-label="Station"');
    expect(html.match(/class="fd-spawn-station"/g)).toHaveLength(2);
    expect(html).toMatch(/checked="" value="a"\/>/);
    expect(html).not.toMatch(/checked="" value="b"/);
    expect(html).toContain('<span class="fd-dot" data-status="online" aria-hidden="true"></span>');
    expect(html).toContain('<span class="fd-spawn-free">3 free</span>');
    // No slot meter line: the pill carries the free count.
    expect(html).not.toContain('fd-slots');
  });

  test('an offline station says so; a station without a limit shows no count', () => {
    const html = pills(
      [station('a', { reachable: false, info: info({ max_sessions: null }) })],
      'a',
    );
    expect(html).toContain('data-status="unreachable"');
    expect(html).toContain('<span class="fd-spawn-off">offline</span>');
    expect(html).not.toContain('free');
  });

  test('a deck that places adds the Auto pill after the stations', () => {
    const html = pills([station('a', { info: info({}) })], AUTO, true);
    expect(html).toMatch(
      new RegExp(`checked="" value="${AUTO}"/>Auto<span class="fd-vh"> \\(best station\\)</span>`),
    );
    expect(pills([station('a', { info: info({}) })], 'a')).not.toContain('Auto');
  });

  test('the head is a breadcrumb with the chosen station; without one, the title only', () => {
    const head = renderToStaticMarkup(<SheetHead station={station('a', { name: 'Lighting' })} />);
    expect(head).toMatch(/<span class="fd-badge">.*Lighting<\/span>/);
    expect(head).toContain('<h2>New session</h2>');
    expect(renderToStaticMarkup(<SheetHead station={undefined} />)).toBe(
      '<div class="fd-spawn-head"><h2>New session</h2></div>',
    );
  });

  test('the footer keeps one line: no missing line without missing steps, the kbd hint in Start', () => {
    const foot = (missing: string[], need: string | null = null, grant: string | null = null) =>
      renderToStaticMarkup(
        <SpawnFoot
          batchShown
          batch={false}
          onBatch={() => {}}
          missing={missing}
          need={need}
          grantProblem={grant}
          describedBy={missing.length === 0 ? undefined : 'fd-spawn-missing'}
          outcome={null}
          onOpenNew={null}
          disabled
          label="Start session"
        />,
      );
    expect(foot([])).not.toContain('data-missing');
    expect(foot([])).toContain(`<span class="fd-kbd" aria-hidden="true">${submitKeyHint()}</span>`);
    expect(foot(['fix the base'])).toContain('data-missing="true">To start: fix the base.</p>');
    // A phone: the directory note sits first in the status slot, above the sticky buttons.
    expect(foot([], 'Choose a folder to start.')).toContain(
      '<div class="fd-spawn-status"><p class="fd-note fd-spawn-need" id="fd-spawn-need">Choose a folder to start.</p></div>',
    );
    expect(foot([])).not.toContain('fd-spawn-need');
    // The grant problem: a hidden copy that only the button description reads.
    expect(foot([], null, 'The session limit is 1 to 50.')).toContain(
      '<span hidden="" id="fd-spawn-grant-error">The session limit is 1 to 50.</span>',
    );
    expect(foot([])).not.toContain('fd-spawn-grant-error');
  });

  test('the kbd hint names the key of the platform: ⌘↵ on Apple systems, else Ctrl+↵', () => {
    const real = Object.getOwnPropertyDescriptor(navigator, 'platform');
    const hint = (platform: string) => {
      Object.defineProperty(navigator, 'platform', { configurable: true, value: platform });
      return submitKeyHint();
    };
    try {
      expect(hint('MacIntel')).toBe('⌘↵');
      expect(hint('iPhone')).toBe('⌘↵');
      expect(hint('iPad')).toBe('⌘↵');
      expect(hint('Linux x86_64')).toBe('Ctrl+↵');
      expect(hint('Win32')).toBe('Ctrl+↵');
    } finally {
      if (real === undefined) delete (navigator as { platform?: string }).platform;
      else Object.defineProperty(navigator, 'platform', real);
    }
  });

  test('the limit badge names the window in words and shows from WARN_PERCENT on (R4.3)', () => {
    const limit = (usedPercent: number, scope = 'five_hour'): RateMeter => ({
      key: 'k',
      stationId: 'station-a',
      instance: 'claude',
      label: 'Claude',
      provider: 'Claude',
      station: 'Alpha',
      scope,
      usedPercent,
      resetsAt: 60 * 60_000,
    });
    expect(limitBadge(limit(42), 0)).toBeNull();
    expect(limitBadge(null, 0)).toBeNull();
    expect(limitBadge(limit(84), 0)).toEqual({
      text: '84% of 5-hour window',
      title: '84% of 5-hour window, resets in 1 h',
    });
    expect(limitBadge(limit(100, 'seven_day'), 0)?.text).toBe('100% of weekly window');
    expect(limitBadge(limit(100, 'seven_day_opus'), 0)?.text).toBe('100% of seven day opus');
    // Each provider choice shows its limit, so the instances compare before a choice.
    const p = { instance: 'work', provider: 'claude', version: '2.1.5' } as StationProvider;
    expect(providerChoice(p, limit(42), 0, true)).toEqual({
      label: 'Claude 2.1.5 (work)',
      suffix: '42% of 5-hour window, resets in 1 h (most room)',
      title: '42% of 5-hour window, resets in 1 h',
    });
    expect(providerChoice(p, null, 0, false)).toEqual({
      label: 'Claude 2.1.5 (work)',
      suffix: '',
      title: undefined,
    });
  });
});
