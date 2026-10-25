// FG-12 (decisions E1): the Taildrive mounts editor of the station settings.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { FieldEditor, GROUP_KINDS } from './FieldEditor';
import { meta, view } from './fixtures';
import { changesOf } from './model';

const noop = () => {};

describe('the Taildrive mounts editor (FG-12)', () => {
  const mounts = meta('taildrive_mounts', 'Taildrive mounts', 'share_mounts');
  const editor = (value: unknown) =>
    renderToStaticMarkup(
      <FieldEditor meta={mounts} value={value} onChange={noop} disabled={false} stationId="st-a" />,
    );

  test('each mount is two inputs, a Remove button, and one Add mount button', () => {
    const html = editor({ 'nas/out': '/mnt/out' });
    expect(html).toContain('aria-label="Share 1"');
    expect(html).toContain('aria-label="Mount path 1"');
    expect(html).toContain('placeholder="nas/out"');
    expect(html).toContain('placeholder="/mnt/out"');
    expect(html).toContain('value="nas/out"');
    expect(html).toContain('value="/mnt/out"');
    expect(html).toContain('data-mono="true"');
    expect(html).toContain('aria-label="Remove Taildrive mounts 1"');
    expect(html).toContain('Add mount');
    expect(editor(undefined)).not.toContain('Share 1');
  });

  test('it is a group kind and its value goes to the deck as a map', () => {
    expect(GROUP_KINDS).toContain('share_mounts');
    const v = view({ fields: [mounts], values: { taildrive_mounts: { 'nas/out': '/mnt/out' } } });
    expect(
      changesOf(v, {
        taildrive_mounts: [
          ['nas/out', '/mnt/out'],
          ['', ''],
        ],
      }),
    ).toEqual({});
    expect(changesOf(v, { taildrive_mounts: [['nas/out', '/mnt/in']] })).toEqual({
      taildrive_mounts: { 'nas/out': '/mnt/in' },
    });
    expect(changesOf(v, { taildrive_mounts: [] })).toEqual({ taildrive_mounts: null });
  });
});
