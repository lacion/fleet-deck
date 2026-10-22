import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { StationResources as Resources } from '../gen';
import { card } from '../testing/fleet';
import { bytesText, StationBusiest, StationResources } from './StationResources';

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

/** A row as markup: the label, the bar, the value and its aside. */
const row = (label: string, value: string, aside: string) =>
  new RegExp(
    `<span class="fd-station-res-label">${label}</span><span class="fd-meter"[^>]*><span[^>]*></span></span><span class="fd-station-res-value">${value} <span class="fd-station-res-aside"><span class="fd-station-res-sep">· </span>${aside}</span></span>`,
  );

describe('the size text of a station card', () => {
  test('MB under 1 GB, one decimal under 100 GB, whole GB under 1 TB, then TB', () => {
    expect(bytesText(0)).toBe('0 MB');
    expect(bytesText(340e6)).toBe('340 MB');
    expect(bytesText(999.6e6)).toBe('1 GB');
    expect(bytesText(1.6e9)).toBe('1.6 GB');
    expect(bytesText(6.0e9)).toBe('6 GB');
    expect(bytesText(46.83e9)).toBe('46.8 GB');
    expect(bytesText(99.96e9)).toBe('100 GB');
    expect(bytesText(217.04e9)).toBe('217 GB');
    expect(bytesText(864.1e9)).toBe('864 GB');
    expect(bytesText(999.6e9)).toBe('1 TB');
    expect(bytesText(1081.1e9)).toBe('1.08 TB');
    expect(bytesText(1998.2e9)).toBe('2 TB');
    expect(bytesText(1.1e12)).toBe('1.1 TB');
  });
});

describe('station resources (DL-193)', () => {
  test('a full report shows a row for each disk, the memory and the CPU', () => {
    const out = html(full);
    expect(out).toContain('aria-label="Resources"');
    expect(out).toContain('data-prop="resources"');
    expect(out.match(/<li/g)?.length).toBe(4);
    expect(out).toMatch(row('Data', '300 GB of 500 GB', '200 GB free'));
    expect(out).toMatch(row('/work', '400 GB of 1 TB', '600 GB free'));
    expect(out).toMatch(row('Memory', '8 GB of 16 GB', '8 GB free'));
    expect(out).toMatch(row('CPU', 'load 2.0', '4 cores'));
    expect(out).toContain('title="/var/data"');
    expect(out.indexOf('>Data<')).toBeLessThan(out.indexOf('>Memory<'));
    expect(out.indexOf('>Memory<')).toBeLessThan(out.indexOf('>CPU<'));
    expect(out).toContain('style="width:60%"');
  });

  test('one core is "1 core"', () => {
    expect(html({ ...full, cpu: { cores: 1, load1_x100: 0, load5_x100: 30 } })).toMatch(
      row('CPU', 'load 0.3', '1 core'),
    );
  });

  test('a missing memory and CPU leave the disk rows only', () => {
    const out = html({ ...full, memory: null, cpu: null });
    expect(out.match(/<li/g)?.length).toBe(2);
    expect(out).not.toContain('>Memory<');
    expect(out).not.toContain('>CPU<');
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
    expect(out.match(/data-level="high"/g)?.length).toBe(1);
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
    const render = (resources: Resources | undefined) =>
      renderToStaticMarkup(
        <StationBusiest resources={resources} station="station-a" sessions={cards} />,
      );

    test('the section is a property of its own with 5 rows in the given order', () => {
      const out = render(busy);
      expect(out).toContain(
        '<div class="fd-station-prop" data-prop="busiest"><h4 class="fd-station-prop-label">Busiest sessions</h4>',
      );
      expect(out).toContain('aria-label="Busiest sessions"');
      expect(out.match(/<li/g)?.length).toBe(5);
      expect(out).not.toContain('s6');
      expect(out.indexOf('s-called')).toBeLessThan(out.indexOf('s-titled'));
      expect(out.indexOf('s-titled')).toBeLessThan(out.indexOf('abcdefgh'));
      // The resource rows are not part of it.
      expect(html(busy)).not.toContain('Busiest sessions');
    });

    test('a row has the callsign (condensed), else the title, else 8 characters of the id', () => {
      const out = render(busy);
      expect(out).toMatch(/<a class="fd-callsign" href="[^"]+">Falcon<\/a>/);
      expect(out).not.toContain('A title');
      expect(out).toMatch(/<a href="[^"]+">Fix the build<\/a>/);
      expect(out).toMatch(/<a href="[^"]+">abcdefgh<\/a>/);
    });

    test('a row shows the CPU with its unit, the memory in GB or MB and a dash for none', () => {
      const out = render(busy);
      expect(out).toContain('<span>340% CPU</span><span>1.2 GB</span>');
      expect(out).toContain('<span>120% CPU</span><span>500 MB</span>');
      expect(out).toContain('<span>—</span><span>—</span>');
    });

    test('a row links to the session', () => {
      expect(render(busy)).toContain('href="#/s/station-a/s-called"');
    });

    test('an empty list or no report shows no section', () => {
      expect(render(full)).toBe('');
      expect(render(undefined)).toBe('');
    });
  });
});
