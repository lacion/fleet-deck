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

describe('strip extras', () => {
  test('a strip with pending mail shows the mail icon with the count', () => {
    const html = board([card('m')], [extra('station-a', 'm', { mail_pending: 2 })]);
    expect(html).toMatch(
      /class="fd-strip-ico" data-ico="mail" title="Mail waiting for delivery"><svg[\s\S]*?<\/svg><span>2<\/span>/,
    );
  });
  test('no mail icon and no touched files without extras', () => {
    const html = board([card('n')], [extra('station-a', 'n')]);
    expect(html).not.toContain('data-ico="mail"');
    expect(html).not.toContain('Touched:');
    expect(html).not.toContain('fd-filechip');
  });
  test('touched files are the basenames in the title of the now cell', () => {
    const html = board(
      [card('f')],
      [extra('station-a', 'f', { files: ['src/app/App.tsx', 'README.md'] })],
    );
    expect(html).toContain('class="fd-strip-now" title="Touched: App.tsx, README.md"');
    expect(html).not.toContain('fd-filechip');
  });
  test('the strip shows no sparkline, also when the board saw events', () => {
    record([card('sp', 'working', 'station-a', { session_seq: 3 })], NOW);
    record([card('sp', 'working', 'station-a', { session_seq: 8 })], NOW);
    const html = board([card('sp')], []);
    expect(html).not.toContain('Activity:');
    expect(html).not.toContain('fd-spark');
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
