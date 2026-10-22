import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { StationInfo } from '../gen';
import { station } from '../testing/fleet';
import { StationReach } from './SpawnSheet';

const info = (extra: Partial<StationInfo>): StationInfo =>
  ({ max_sessions: 4, live_sessions: 1, ...extra }) as StationInfo;

describe('the New session sheet (DESIGN-SYSTEM §4.6)', () => {
  test('the chosen station shows its reach, its slots and the free count', () => {
    const html = renderToStaticMarkup(<StationReach station={station('a', { info: info({}) })} />);
    expect(html).toContain('class="fd-spawn-reach" data-up="true"');
    expect(html).toContain('online');
    expect(html.match(/data-slot="used"/g)).toHaveLength(1);
    expect(html).toContain('<span class="fd-spawn-free">3 free</span>');
  });

  test('an offline station says so; a station without a limit shows no count', () => {
    const html = renderToStaticMarkup(
      <StationReach
        station={station('a', { reachable: false, info: info({ max_sessions: null }) })}
      />,
    );
    expect(html).toContain('data-up="false"');
    expect(html).toContain('offline');
    expect(html).not.toContain('fd-slots');
    expect(html).not.toContain('free');
  });

  test('a large station shows the count without the slot ticks', () => {
    const html = renderToStaticMarkup(
      <StationReach
        station={station('a', { info: info({ max_sessions: 40, live_sessions: 2 }) })}
      />,
    );
    expect(html).not.toContain('fd-slots');
    expect(html).toContain('38 free');
  });

  test('no station: nothing', () => {
    expect(renderToStaticMarkup(<StationReach station={undefined} />)).toBe('');
  });
});
