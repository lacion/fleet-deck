// B27 §3.7: the control of a `limit` field.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { limitMeta, view } from './fixtures';
import { LimitEditor, limitAfterToggle } from './LimitEditor';
import { SettingsFields } from './SettingsFields';

const noop = () => {};
const spawnCap = limitMeta('grant_limits.spawn_cap', 'Highest session limit of a grant', 1, 100000);
const idle = limitMeta('grant_limits.idle_hours', 'Longest idle time of a grant (hours)', 1, 168);

const editor = (value: unknown, disabled = false) =>
  renderToStaticMarkup(
    <LimitEditor meta={spawnCap} value={value} disabled={disabled} onChange={noop} />,
  );

describe('the limit editor', () => {
  test('a number shows in the input, the box is clear', () => {
    const html = editor(50);
    expect(html).toContain('value="50"');
    expect(html).toContain('No limit');
    expect(html).not.toContain('checked=""');
    expect(html).not.toMatch(/<input[^>]*type="number"[^>]*disabled/);
  });

  test('the input respects min and max', () => {
    const html = editor(50);
    expect(html).toContain('min="1"');
    expect(html).toContain('max="100000"');
  });

  test('"none" checks the box and disables the input', () => {
    const html = editor('none');
    expect(html).toMatch(/<input[^>]*type="checkbox"[^>]*checked=""/);
    expect(html).toMatch(/<input[^>]*type="number"[^>]*disabled=""/);
    expect(html).toContain('value=""');
  });

  test('a disabled field disables both controls', () => {
    const html = editor(5, true);
    expect(html.match(/disabled=""/g)?.length).toBe(2);
  });

  test('the box sends "none"; clearing it restores the last number', () => {
    expect(limitAfterToggle(true, 50, 1)).toBe('none');
    expect(limitAfterToggle(false, 50, 1)).toBe(50);
    // A field that never had a number gets its lowest allowed number.
    expect(limitAfterToggle(false, null, 5)).toBe(5);
    expect(limitAfterToggle(false, null, null)).toBe(1);
  });

  test('a limit field in the page uses the editor, with its unit', () => {
    const html = renderToStaticMarkup(
      <SettingsFields
        view={view({ fields: [idle], values: { 'grant_limits.idle_hours': 24 } })}
        edits={{}}
        errors={{}}
        onEdit={noop}
        stationId={null}
        savedIds={[]}
        secretBlocked={null}
        onSecret={noop}
      />,
    );
    expect(html).toContain('Grant limits');
    expect(html).toContain('aria-label="Longest idle time of a grant in hours"');
    expect(html).toContain('No limit');
  });
});
