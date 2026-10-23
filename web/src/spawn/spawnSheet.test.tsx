import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { RateMeter } from '../board/usage';
import type { StationInfo, StationProvider } from '../gen';
import { station } from '../testing/fleet';
import { submitKeyHint } from './keyHint';
import { AUTO, limitBadge, providerChoice } from './spawn';
import { OptionsSummary, optionsHint, SheetHead, SpawnFoot, StationPills } from './SpawnSheet';

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

  test('a deck that places adds the Auto pill first, with a sparkles icon and a title', () => {
    const html = pills([station('a', { info: info({}) })], AUTO, true);
    expect(html).toMatch(
      new RegExp(
        `^<div class="fd-spawn-stations" role="radiogroup" aria-label="Station"><label class="fd-spawn-station" title="The deck picks the station that fits best"><input type="radio" name="[^"]+" checked="" value="${AUTO}"/><svg[^>]*lucide-sparkles[^>]*>.*?</svg>Auto<span class="fd-vh"> \\(best station\\)</span></label><label class="fd-spawn-station">`,
      ),
    );
    // A station pill has no title: its name and its reach are its text.
    expect(html.match(/ title="/g)).toHaveLength(1);
    expect(pills([station('a', { info: info({}) })], 'a')).not.toContain('Auto');
  });

  test('the head is a breadcrumb with the chosen station or Auto; without one, the title only', () => {
    const head = renderToStaticMarkup(
      <SheetHead station={station('a', { name: 'Lighting' })} auto={false} />,
    );
    expect(head).toMatch(/<span class="fd-badge">.*Lighting<\/span>/);
    expect(head).toContain('<h2>New session</h2>');
    // Auto keeps the badge and the chevron, so that "New session" does not move.
    const auto = renderToStaticMarkup(<SheetHead station={undefined} auto />);
    expect(auto).toMatch(
      /^<div class="fd-spawn-head"><span class="fd-badge"><svg[^>]*>.*<\/svg>Auto<\/span><span class="fd-spawn-crumb" aria-hidden="true">.*<h2>New session<\/h2><\/div>$/,
    );
    expect(auto).not.toContain('Lighting');
    expect(renderToStaticMarkup(<SheetHead station={undefined} auto={false} />)).toBe(
      '<div class="fd-spawn-head"><h2>New session</h2></div>',
    );
  });

  test('the footer keeps one line: no missing line without missing steps, the kbd hint in Start', () => {
    const foot = (missing: string[], grant: string | null = null, batchShown = true) =>
      renderToStaticMarkup(
        <SpawnFoot
          batchShown={batchShown}
          batch={false}
          onBatch={() => {}}
          missing={missing}
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
    // Nothing to say: an empty status slot (a phone hides it), and the directory note is never
    // here: it stays under its field.
    expect(foot([])).toContain('<div class="fd-spawn-status"></div>');
    expect(foot([])).not.toContain('fd-spawn-need');
    // The Batch switch at the left; none for a clone or on a phone (it sits in "More options").
    expect(foot([])).toContain('<label class="fd-spawn-batch">');
    expect(foot([], null, false)).not.toContain('Batch');
    // The grant problem: a hidden copy that only the button description reads.
    expect(foot([], 'The session limit is 1 to 50.')).toContain(
      '<span hidden="" id="fd-spawn-grant-error">The session limit is 1 to 50.</span>',
    );
    expect(foot([])).not.toContain('fd-spawn-grant-error');
  });

  test('"More options" names what it holds in a dim hint', () => {
    expect(optionsHint({ batch: false, setup: false })).toBe('Coordinator, import');
    expect(optionsHint({ batch: false, setup: true })).toBe('Setup, coordinator, import');
    expect(optionsHint({ batch: true, setup: true })).toBe('Batch, setup, coordinator, import');
    // The name keeps an element of its own (`getByText('More options', { exact: true })`).
    expect(
      renderToStaticMarkup(<OptionsSummary hint="Coordinator, import" onToggle={() => {}} />),
    ).toBe(
      '<summary class="fd-label"><span>More options</span><span class="fd-spawn-options-hint">Coordinator, import</span></summary>',
    );
  });

  test('the kbd hint names the key of the platform: ⌘↵ on Apple systems, else Ctrl+Enter', () => {
    const real = Object.getOwnPropertyDescriptor(navigator, 'platform');
    const hint = (platform: string) => {
      Object.defineProperty(navigator, 'platform', { configurable: true, value: platform });
      return submitKeyHint();
    };
    try {
      expect(hint('MacIntel')).toBe('⌘↵');
      expect(hint('iPhone')).toBe('⌘↵');
      expect(hint('iPad')).toBe('⌘↵');
      expect(hint('Linux x86_64')).toBe('Ctrl+Enter');
      expect(hint('Win32')).toBe('Ctrl+Enter');
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
    // Each provider choice shows its limit, so the instances compare before a choice. The CLI
    // version is in the title, not in the name.
    const p = { instance: 'work', provider: 'claude', version: '2.1.5' } as StationProvider;
    expect(providerChoice(p, limit(42), 0, true)).toEqual({
      label: 'Claude (work)',
      suffix: '42% of 5-hour window, resets in 1 h (most room)',
      title: 'Claude 2.1.5 · 42% of 5-hour window, resets in 1 h',
    });
    expect(providerChoice(p, null, 0, false)).toEqual({
      label: 'Claude (work)',
      suffix: '',
      title: 'Claude 2.1.5',
    });
    // A version that the station did not report is left out: no "version unknown".
    expect(providerChoice({ ...p, version: null }, null, 0, false)).toEqual({
      label: 'Claude (work)',
      suffix: '',
      title: undefined,
    });
  });
});
