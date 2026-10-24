// The settings controls of the keys with a fixed value form: the offline approval policy, the
// worker grant stations and the idle stop minutes. They write the same wire values as before.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { station } from '../testing/fleet';
import { FieldEditor } from './FieldEditor';
import { meta } from './fixtures';
import { parsePolicy, policyText, StationsEditor, stationRows } from './SpecialEditors';

const noop = () => {};

describe('the offline approval policy', () => {
  test('parse and write keep the wire value', () => {
    expect(parsePolicy('wait')).toEqual({ mode: 'wait' });
    expect(parsePolicy('')).toEqual({ mode: 'wait' });
    expect(parsePolicy(' deny_after(5m) ')).toEqual({ mode: 'deny', n: '5', unit: 'm' });
    expect(parsePolicy('deny_after(90s)')).toEqual({ mode: 'deny', n: '90', unit: 's' });
    expect(parsePolicy('deny_after(2h)')).toEqual({ mode: 'deny', n: '2', unit: 'h' });
    expect(parsePolicy('later')).toBeNull();
    for (const text of ['wait', 'deny_after(5m)', 'deny_after(90s)', 'deny_after(2h)']) {
      const p = parsePolicy(text);
      expect(p === null ? null : policyText(p)).toBe(text);
    }
  });

  const editor = (value: string) =>
    renderToStaticMarkup(
      <FieldEditor
        meta={meta('offline_approval_policy', 'Approvals while no deck is connected', 'text')}
        value={value}
        disabled={false}
        stationId="st-alpha"
        onChange={noop}
      />,
    );

  test('wait is a choice with no time', () => {
    const html = editor('wait');
    expect(html).toContain('aria-label="When no deck is connected"');
    expect(html).toContain('<option value="wait" selected="">Keep the request open</option>');
    expect(html).toContain('<option value="deny">Deny it after a time</option>');
    expect(html).not.toContain('aria-label="Time"');
  });

  test('deny after a time shows the number and the unit', () => {
    const html = editor('deny_after(5m)');
    expect(html).toContain('<option value="deny" selected="">Deny it after a time</option>');
    expect(html).toMatch(/<input type="number"[^>]*aria-label="Time"[^>]*value="5"/);
    expect(html).toContain('<option value="m" selected="">minutes</option>');
  });

  test('a text that the board does not know stays a text field', () => {
    expect(editor('later')).toContain('value="later"');
  });
});

describe('the worker grant stations', () => {
  const stations = [station('station-a'), station('station-b')];
  const names = stations.map((s) => s.name);
  const editor = (value: string[]) =>
    renderToStaticMarkup(
      <StationsEditor value={value} stations={stations} disabled={false} onChange={noop} />,
    );

  test('"all" is the one checkbox, checked', () => {
    const html = editor(['all']);
    expect(html).toMatch(/<input type="checkbox" checked=""\/>All paired stations/);
    for (const name of names) expect(html).not.toContain(`>${name}<`);
  });

  test('names: one checkbox for each station; a name that is not paired says so', () => {
    const first = names[0] ?? '';
    const html = editor([first, 'gone']);
    expect(html).toMatch(/<input type="checkbox"\/>All paired stations/);
    for (const name of names) expect(html).toContain(`>${name}<`);
    expect(html).toContain('>gone</span><span class="fd-set-faint">not paired</span>');
    expect(stationRows(stations, [first, 'gone'])).toEqual([
      { name: first, paired: true },
      { name: names[1] ?? '', paired: true },
      { name: 'gone', paired: false },
    ]);
  });

  test('the last checked station cannot be unchecked: an empty list would mean every station', () => {
    const first = names[0] ?? '';
    const html = editor([first]);
    expect(html).toMatch(
      /title="A grant needs one station or more\."><input type="checkbox" disabled="" checked=""/,
    );
  });

  test('the field uses the picker when the page has the stations', () => {
    const field = meta('worker_grant.stations', 'Worker grant stations', 'texts');
    const html = renderToStaticMarkup(
      <FieldEditor
        meta={field}
        value={['all']}
        disabled={false}
        stationId={null}
        stations={stations}
        onChange={noop}
      />,
    );
    expect(html).toContain('All paired stations');
    const plain = renderToStaticMarkup(
      <FieldEditor
        meta={field}
        value={['all']}
        disabled={false}
        stationId={null}
        onChange={noop}
      />,
    );
    expect(plain).toContain('value="all"');
  });
});

describe('the idle stop minutes', () => {
  const editor = (value: number) =>
    renderToStaticMarkup(
      <FieldEditor
        meta={meta('hibernate_after_min', 'Stop idle sessions after (minutes)', 'int', {
          min: 0,
          max: 10080,
        })}
        value={value}
        disabled={false}
        stationId="st-alpha"
        onChange={noop}
      />,
    );

  test('0 is the empty field with "Never"; a time shows its minutes and the unit', () => {
    const off = editor(0);
    expect(off).toMatch(
      /aria-label="Stop idle sessions after \(minutes\)" placeholder="Never"[^>]*value=""/,
    );
    expect(off).toContain('>min</span>');
    expect(editor(30)).toMatch(/value="30"/);
  });
});
