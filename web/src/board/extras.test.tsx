import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { SessionExtras } from '../gen';
import { card, station } from '../testing/fleet';
import { record } from './activity';
import { FleetBoard } from './FleetBoard';
import { Sparkline } from './Sparkline';

const NOW = 1_760_000_040_000;

const extra = (station_id: string, session_id: string, over: Partial<SessionExtras> = {}) =>
  ({ station_id, session_id, mail_pending: 0, files: [], ...over }) satisfies SessionExtras;

const board = (
  sessions: ReturnType<typeof card>[],
  extras: SessionExtras[],
  contested?: ReadonlyMap<string, { with: string[]; files: string[] }>,
) =>
  renderToStaticMarkup(
    <FleetBoard
      sessions={sessions}
      stations={[station()]}
      freshness="live"
      now={NOW}
      contested={contested}
      extras={new Map(extras.map((x) => [`${x.station_id}/${x.session_id}`, x]))}
    />,
  );

describe('card extras', () => {
  test('a card with pending mail shows the badge with the count', () => {
    const html = board([card('m')], [extra('station-a', 'm', { mail_pending: 2 })]);
    expect(html).toContain('<span class="fd-card-mail" title="Mail waiting for delivery">');
    expect(html).toMatch(/class="fd-card-mail"[^>]*>[\s\S]*?<\/svg> 2<\/span>/);
  });
  test('no mail badge and no chip line without extras', () => {
    const html = board([card('n')], [extra('station-a', 'n')]);
    expect(html).not.toContain('fd-card-mail');
    expect(html).not.toContain('fd-card-files');
  });
  test('touched files show as chips with the basename and the path as title', () => {
    const html = board(
      [card('f')],
      [extra('station-a', 'f', { files: ['src/app/App.tsx', 'README.md'] })],
    );
    expect(html).toContain('<span class="fd-card-files">');
    expect(html).toContain('title="src/app/App.tsx">App.tsx</span>');
    expect(html).toContain('title="README.md">README.md</span>');
    expect(html).not.toContain('data-hot');
  });
  test('a contested file is a hot chip', () => {
    const html = board(
      [card('h')],
      [extra('station-a', 'h', { files: ['src/a.ts', 'src/b.ts'] })],
      new Map([['station-a/h', { with: ['other'], files: ['src/b.ts'] }]]),
    );
    expect(html.match(/data-hot="true"/g)).toHaveLength(1);
    expect(html).toMatch(/title="src\/b\.ts" data-hot="true">b\.ts</);
  });
  test('the card shows the sparkline when the board saw events', () => {
    record([card('sp', 'working', 'station-a', { session_seq: 3 })], NOW);
    record([card('sp', 'working', 'station-a', { session_seq: 8 })], NOW);
    const html = board([card('sp')], []);
    expect(html).toContain('aria-label="Activity: 5 events in the last 20 minutes"');
    expect(html).not.toContain('Activity: 0 events');
  });
});

describe('Sparkline', () => {
  const values = [...Array.from({ length: 18 }, () => 0), 4, 2];
  test('draws a bar for each non-zero value, the last one marked', () => {
    const html = renderToStaticMarkup(<Sparkline values={values} />);
    expect(html).toContain('width="59"');
    expect(html).toContain('height="14"');
    expect(html).toContain('role="img"');
    expect(html.match(/<rect/g)).toHaveLength(2);
    expect(html.match(/data-last="true"/g)).toHaveLength(1);
  });
  test('the biggest bar is 14 px and a small one is at least 1 px', () => {
    const html = renderToStaticMarkup(
      <Sparkline values={[...Array.from({ length: 19 }, () => 0), 1]} />,
    );
    expect(html).toContain('height="14"');
    const small = renderToStaticMarkup(
      <Sparkline values={[1000, ...Array.from({ length: 18 }, () => 0), 1]} />,
    );
    expect(small).toContain('height="1"');
  });
});
