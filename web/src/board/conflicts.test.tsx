import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { ConflictRadar } from './ConflictRadar';
import { agoText, conflictLoader, conflictView } from './conflicts';

const conflict = {
  repo: 'github.com/o/app',
  file: 'src/lib.rs',
  touches: [
    {
      station_id: 'st-b',
      session_id: 's2',
      session_title: 'Beta fix',
      path: 'lib.rs',
      touched_at: 2_000,
    },
    {
      station_id: 'st-a',
      session_id: 's1',
      session_title: null,
      path: '/x/src/lib.rs',
      touched_at: 1_000,
    },
  ],
};

describe('conflict radar', () => {
  test('the loader reads GET /api/conflicts and refuses a bad row', async () => {
    const paths: string[] = [];
    const load = conflictLoader(async (p) => {
      paths.push(p);
      return [conflict];
    });
    expect(await load()).toEqual([conflict]);
    expect(paths).toEqual(['/api/conflicts']);
    expect(() => conflictView({ ...conflict, touches: [{ path: 1 }] })).toThrow(
      'The deck sent an invalid conflict.',
    );
    await expect(conflictLoader(async () => ({}))()).rejects.toThrow('invalid conflict list');
  });

  test('ages read in minutes', () => {
    expect(agoText(1_000, 1_000)).toBe('just now');
    expect(agoText(0, 5 * 60_000)).toBe('5 min ago');
  });

  test('the radar renders nothing until the first answer, and nothing without conflicts', () => {
    const html = renderToStaticMarkup(
      <ConflictRadar list={null} problem={null} stations={[]} onOpen={() => {}} />,
    );
    expect(html).toBe('');
  });
});
