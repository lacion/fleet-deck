import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { StationResources as Resources } from '../gen';
import { StationResources } from './StationResources';

const full: Resources = {
  at: 1,
  disks: [
    { label: 'Data', path: '/var/data', total_bytes: 500e9, free_bytes: 200e9 },
    { label: '/work', path: '/work', total_bytes: 1e12, free_bytes: 600e9 },
  ],
  memory: { total_bytes: 16e9, available_bytes: 8e9 },
  cpu: { cores: 4, load1_x100: 100, load5_x100: 200 },
};

const html = (resources: Resources | undefined) =>
  renderToStaticMarkup(<StationResources resources={resources} />);

describe('station resources (DL-193)', () => {
  test('a full report shows a row for each disk, the memory and the CPU', () => {
    const out = html(full);
    expect(out).toContain('aria-label="Resources"');
    expect(out.match(/<li/g)?.length).toBe(4);
    expect(out).toContain('Data: 200.0 GB free of 500.0 GB');
    expect(out).toContain('/work: 600.0 GB free of 1000.0 GB');
    expect(out).toContain('Memory: 8.0 GB free of 16.0 GB');
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
      disks: [{ label: 'Data', path: '/var/data', total_bytes: 500e9, free_bytes: 3e9 }],
    });
    expect(out.match(/data-low/g)?.length).toBe(1);
    expect(out).toMatch(/<li data-low="" title="\/var\/data">/);
    expect(out).toContain('data-high=""');
  });

  test('the warning line shows only with a warning', () => {
    expect(html(full)).not.toContain('Low resources');
    const low = html({ ...full, cpu: { cores: 4, load1_x100: 0, load5_x100: 1000 } });
    expect(low).toContain('Low resources. New sessions can be slow or fail here.');
    expect(low).toContain('role="status"');
  });

  test('no report renders nothing', () => {
    expect(html(undefined)).toBe('');
  });
});
