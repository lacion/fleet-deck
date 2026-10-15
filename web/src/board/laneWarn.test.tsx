import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { StationInfo, StationResources } from '../gen';
import { card, station } from '../testing/fleet';
import { FleetBoard } from './FleetBoard';

const lowDisk: StationResources = {
  at: 1,
  disks: [{ label: 'Data', path: '/var/data', total_bytes: 500e9, free_bytes: 3_200_000_000 }],
  memory: null,
  cpu: null,
};

const info = (resources?: StationResources): StationInfo => ({
  name: 'Alpha',
  incarnation: 'inc',
  head: 0,
  live_sessions: 0,
  version: '0.2.0',
  max_sessions: 4,
  allow_shell: null,
  providers: [],
  ...(resources === undefined ? {} : { resources }),
});

const board = (resources: StationResources | undefined, group: 'station' | 'repo') =>
  renderToStaticMarkup(
    <FleetBoard
      sessions={[card('a')]}
      stations={[station('station-a', { info: info(resources), info_at: 1 })]}
      freshness="live"
      group={group}
    />,
  );

describe('fleet lane: low resources (DL-193)', () => {
  test('a station lane with a low disk shows the chip with the warning in the title', () => {
    const html = board(lowDisk, 'station');
    expect(html).toContain('▲ Low resources');
    expect(html).toContain('title="Low disk: 3.2 GB free on Data"');
  });

  test('a station lane without resources shows no chip', () => {
    expect(board(undefined, 'station')).not.toContain('Low resources');
  });

  test('the repository grouping shows no chip', () => {
    expect(board(lowDisk, 'repo')).not.toContain('Low resources');
  });
});
