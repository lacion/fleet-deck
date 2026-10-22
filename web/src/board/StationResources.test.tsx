import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { StationResources as Resources } from '../gen';
import { card } from '../testing/fleet';
import { StationResources } from './StationResources';

const full: Resources = {
  at: 1,
  disks: [
    { label: 'Data', path: '/var/data', total_bytes: 500e9, free_bytes: 200e9 },
    { label: '/work', path: '/work', total_bytes: 1e12, free_bytes: 600e9 },
  ],
  memory: { total_bytes: 16e9, available_bytes: 8e9 },
  cpu: { cores: 4, load1_x100: 100, load5_x100: 200 },
  sessions: [],
};

const html = (resources: Resources | undefined) =>
  renderToStaticMarkup(<StationResources resources={resources} />);

describe('station resources (DL-193)', () => {
  test('a full report shows a row for each disk, the memory and the CPU', () => {
    const out = html(full);
    expect(out).toContain('aria-label="Resources"');
    expect(out.match(/<li/g)?.length).toBe(4);
    expect(out).toContain('Data: 300.0 GB used of 500.0 GB · 200.0 GB free');
    expect(out).toContain('/work: 400.0 GB used of 1000.0 GB · 600.0 GB free');
    expect(out).toContain('Memory: 8.0 GB used of 16.0 GB · 8.0 GB free');
    expect(out).toContain('CPU: load 2.0 on 4 cores');
    expect(out).toContain('title="/var/data"');
    expect(out.indexOf('Data:')).toBeLessThan(out.indexOf('Memory:'));
    expect(out.indexOf('Memory:')).toBeLessThan(out.indexOf('CPU:'));
    expect(out).toContain('style="width:60%"');
  });

  test('a missing memory and CPU leave the disk rows only', () => {
    const out = html({ ...full, memory: null, cpu: null });
    expect(out.match(/<li/g)?.length).toBe(2);
    expect(out).not.toContain('Memory:');
    expect(out).not.toContain('CPU:');
  });

  test('a low disk row has data-low and the others do not', () => {
    const out = html({
      ...full,
      disks: [
        {
          label: 'Data',
          path: '/var/data',
          total_bytes: 500e9,
          free_bytes: 3e9,
        },
      ],
    });
    expect(out.match(/data-low/g)?.length).toBe(1);
    expect(out).toMatch(/<li data-low="" title="\/var\/data">/);
    expect(out).toContain('data-high=""');
  });

  test('the warning line shows only with a warning', () => {
    expect(html(full)).not.toContain('Low resources');
    const low = html({
      ...full,
      cpu: { cores: 4, load1_x100: 0, load5_x100: 1000 },
    });
    expect(low).toContain('Low resources. New sessions can be slow or fail here.');
    expect(low).toContain('role="status"');
  });

  test('no report renders nothing', () => {
    expect(html(undefined)).toBe('');
  });

  describe('the busiest sessions (R13 §10)', () => {
    const load = (id: string, cpu: number | null, rss: number | null) => ({
      session_id: id,
      cpu_pct: cpu,
      rss_bytes: rss,
      procs: 3,
    });
    const cards = [
      card('s-called', 'working', 'station-a', {
        callsign: 'Falcon',
        title: 'A title',
      }),
      card('s-titled', 'working', 'station-a', {
        callsign: null,
        title: 'Fix the build',
      }),
    ];
    const busy: Resources = {
      ...full,
      sessions: [
        load('s-called', 340, 1.2e9),
        load('s-titled', 120, 500e6),
        load('abcdefghijkl', null, null),
        load('s4', 10, 1e9),
        load('s5', 5, 1e9),
        load('s6', 1, 1e9),
      ],
    };
    const render = (resources: Resources) =>
      renderToStaticMarkup(
        <StationResources resources={resources} station="station-a" sessions={cards} />,
      );

    test('the section shows 5 rows in the given order', () => {
      const out = render(busy);
      expect(out).toContain('aria-label="Busiest sessions"');
      const list = out.slice(out.indexOf('aria-label="Busiest sessions"'));
      expect(list.match(/<li/g)?.length).toBe(5);
      expect(list).not.toContain('s6');
      expect(list.indexOf('s-called')).toBeLessThan(list.indexOf('s-titled'));
      expect(list.indexOf('s-titled')).toBeLessThan(list.indexOf('abcdefgh'));
    });

    test('a row has the callsign, else the title, else 8 characters of the id', () => {
      const out = render(busy);
      expect(out).toContain('>Falcon</a>');
      expect(out).not.toContain('A title');
      expect(out).toContain('>Fix the build</a>');
      expect(out).toContain('>abcdefgh</a>');
    });

    test('a row shows the CPU as a percent, the memory in GB or MB and a dash for none', () => {
      const out = render(busy);
      expect(out).toContain('<span>340%</span><span>1.2 GB</span>');
      expect(out).toContain('<span>120%</span><span>500 MB</span>');
      expect(out).toContain('<span>—</span><span>—</span>');
    });

    test('a row links to the session', () => {
      expect(render(busy)).toContain('href="#/s/station-a/s-called"');
    });

    test('an empty list shows no section', () => {
      const out = render(full);
      expect(out).not.toContain('Busiest sessions');
      expect(out.match(/<li/g)?.length).toBe(4);
    });
  });
});
