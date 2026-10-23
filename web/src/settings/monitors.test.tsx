import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { MonitorInfo } from '../gen';
import { meta, view } from './fixtures';
import { SettingsFields } from './SettingsFields';

const noop = () => {};

const monitor = (name: string, description: string): MonitorInfo => ({
  name,
  description,
  width: 1920,
  height: 1080,
  scale_pct: 100,
  refresh_hz: 60,
  focused: false,
  dpms_on: true,
});

const MONITORS = [monitor('DP-1', 'Dell U2720Q'), monitor('HDMI-A-1', '')];

const fields = (
  values: Record<string, unknown>,
  monitors?: readonly MonitorInfo[],
  stationId: string | null = 'st-alpha',
) =>
  renderToStaticMarkup(
    <SettingsFields
      view={view({
        fields: [
          meta('capture_monitors', 'Screenshot monitors', 'texts', {
            help: 'Help of monitors.',
            danger: true,
          }),
          meta('member_deny', 'Hidden file patterns', 'texts'),
        ],
        values: { capture_monitors: [], member_deny: ['*.pem'], ...values },
        overrides: [],
      })}
      edits={{}}
      errors={{}}
      onEdit={noop}
      stationId={stationId}
      monitors={monitors}
      savedIds={[]}
      secretBlocked={null}
      onSecret={noop}
    />,
  );

describe('the monitor drop-downs of capture_monitors', () => {
  test('each row is a select with every monitor as name and description', () => {
    const html = fields({ capture_monitors: ['DP-1'] }, MONITORS);
    expect(html).toContain('<select aria-label="Screenshot monitors 1"');
    expect(html).not.toContain('<input aria-label="Screenshot monitors 1"');
    expect(html).toContain('<option value="DP-1" selected="">DP-1 — Dell U2720Q</option>');
    expect(html).toContain('<option value="HDMI-A-1">HDMI-A-1</option>');
    expect(html).toContain('aria-label="Remove Screenshot monitors 1"');
    expect(html).toContain('The monitors that a screenshot may capture.');
    expect(html).not.toContain('This station reports no monitors');
  });

  test('a value that no monitor has is the first option as not connected', () => {
    const html = fields({ capture_monitors: ['DP-9'] }, MONITORS);
    expect(html).toContain(
      '<option value="DP-9" selected="">DP-9 (not connected)</option><option value="DP-1">',
    );
  });

  test('Add is enabled while a monitor is unused and disabled with the title when all are used', () => {
    const some = fields({ capture_monitors: ['DP-1'] }, MONITORS);
    expect(some).not.toContain('Every monitor is in the list.');
    const all = fields({ capture_monitors: ['DP-1', 'HDMI-A-1'] }, MONITORS);
    expect(all).toContain('disabled="" title="Every monitor is in the list."');
  });

  test('the other texts field keeps its input', () => {
    const html = fields({ capture_monitors: ['DP-1'] }, MONITORS);
    expect(html).toContain('<input aria-label="Hidden file patterns 1"');
    expect(html).not.toContain('<select aria-label="Hidden file patterns 1"');
  });

  test.each([
    ['no monitors', [] as readonly MonitorInfo[], 'st-alpha'],
    ['no desktop', undefined, 'st-alpha'],
    ['the deck tab', undefined, null],
  ])('with %s the rows are text inputs and the help has the extra sentence', (_, monitors, id) => {
    const html = fields({ capture_monitors: ['DP-1'] }, monitors, id);
    expect(html).toContain('<input aria-label="Screenshot monitors 1"');
    expect(html).not.toContain('<select aria-label="Screenshot monitors 1"');
    expect(html).toContain(
      'Leave it empty to allow every monitor. This station reports no monitors, so type the names.',
    );
  });
});
