import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { RateMeter } from '../board/usage';
import type { StationInfo } from '../gen';
import { station } from '../testing/fleet';
import { AUTO, limitBadge } from './spawn';
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
    const foot = (missing: string[]) =>
      renderToStaticMarkup(
        <SpawnFoot
          batchShown
          batch={false}
          onBatch={() => {}}
          missing={missing}
          describedBy={missing.length === 0 ? undefined : 'fd-spawn-missing'}
          outcome={null}
          onOpenNew={null}
          disabled
          label="Start session"
        />,
      );
    expect(foot([])).not.toContain('data-missing');
    expect(foot([])).toContain('<span class="fd-kbd" aria-hidden="true">⌘↵</span>');
    expect(foot(['fix the base'])).toContain('data-missing="true">To start: fix the base.</p>');
  });

  test('the limit badge names the window in words and shows from WARN_PERCENT on (R4.3)', () => {
    const limit = (usedPercent: number, scope = 'five_hour'): RateMeter => ({
      key: 'k',
      stationId: 'station-a',
      instance: 'claude',
      label: 'Claude',
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
    expect(limitBadge(limit(100, 'seven_day'), 0)?.text).toBe('100% of seven day');
  });
});
