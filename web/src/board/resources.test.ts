import { describe, expect, test } from 'bun:test';
import type { StationResources } from '../gen';
import {
  cpuBusy,
  gb,
  isLow,
  LOW_DISK_BYTES,
  LOW_MEMORY_BYTES,
  resourceWarnings,
  usedShare,
} from './resources';

const report = (extra: Partial<StationResources> = {}): StationResources => ({
  at: 1,
  disks: [],
  memory: null,
  cpu: null,
  ...extra,
});

describe('station resource rules (DL-193)', () => {
  test('a disk is low under 10 GB free', () => {
    expect(isLow(9_999_999_999, 50_000_000_000, LOW_DISK_BYTES)).toBe(true);
    expect(isLow(10_000_000_000, 50_000_000_000, LOW_DISK_BYTES)).toBe(false);
  });

  test('a disk is low under 10 % free', () => {
    expect(isLow(50e9, 600e9, LOW_DISK_BYTES)).toBe(true);
    expect(isLow(60e9, 600e9, LOW_DISK_BYTES)).toBe(false);
  });

  test('a total of 0 is never low', () => {
    expect(isLow(0, 0, LOW_DISK_BYTES)).toBe(false);
  });

  test('memory is low under 1 GB available', () => {
    expect(isLow(999_999_999, 8_000_000_000, LOW_MEMORY_BYTES)).toBe(true);
    expect(isLow(1_000_000_000, 8_000_000_000, LOW_MEMORY_BYTES)).toBe(false);
  });

  test('the CPU is busy above 1.5 load per core and never without cores', () => {
    expect(cpuBusy({ cores: 8, load1_x100: 0, load5_x100: 1200 })).toBe(false);
    expect(cpuBusy({ cores: 8, load1_x100: 0, load5_x100: 1201 })).toBe(true);
    expect(cpuBusy({ cores: 0, load1_x100: 0, load5_x100: 5000 })).toBe(false);
  });

  test('sizes are decimal GB with one decimal', () => {
    expect(gb(3_200_000_000)).toBe('3.2 GB');
    expect(gb(0)).toBe('0.0 GB');
  });

  test('the warnings come in order with the exact texts', () => {
    const warnings = resourceWarnings(
      report({
        disks: [
          { label: 'Data', path: '/data', total_bytes: 500e9, free_bytes: 3_200_000_000 },
          { label: 'Fine', path: '/fine', total_bytes: 1e12, free_bytes: 900e9 },
          { label: '/work', path: '/work', total_bytes: 1e12, free_bytes: 5e9 },
        ],
        memory: { total_bytes: 16e9, available_bytes: 500_000_000 },
        cpu: { cores: 4, load1_x100: 0, load5_x100: 1250 },
      }),
    );
    expect(warnings).toEqual([
      { kind: 'disk', text: 'Low disk: 3.2 GB free on Data' },
      { kind: 'disk', text: 'Low disk: 5.0 GB free on /work' },
      { kind: 'memory', text: 'Low memory: 0.5 GB free of 16.0 GB' },
      { kind: 'cpu', text: 'Busy CPU: load 12.5 on 4 cores' },
    ]);
  });

  test('no report gives no warnings', () => {
    expect(resourceWarnings(undefined)).toEqual([]);
    expect(resourceWarnings(null)).toEqual([]);
    expect(resourceWarnings(report())).toEqual([]);
  });

  test('the used share stays within 0 and 1', () => {
    expect(usedShare(5, 10)).toBe(0.5);
    expect(usedShare(20, 10)).toBe(1);
    expect(usedShare(-5, 10)).toBe(0);
    expect(usedShare(5, 0)).toBe(0);
    expect(usedShare(5, -1)).toBe(0);
  });
});
