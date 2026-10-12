import { describe, expect, test } from 'bun:test';
import { cells, newId, shellActions, shellList } from './shell';

const info = {
  shell_id: 'shell-a',
  cwd: '/w',
  state: 'running',
  cols: 80,
  rows: 24,
  created_at: 1,
  exit_code: null,
};

describe('shell calls', () => {
  test('open, resize and close post audited commands with fresh ids', async () => {
    const posts: [string, unknown][] = [];
    const gets: string[] = [];
    let n = 0;
    const actions = shellActions(
      async (path) => {
        gets.push(path);
        return [info];
      },
      async (path, body) => {
        posts.push([path, body]);
        return { state: 'accepted' };
      },
      () => {
        n = (n + 1) % 36;
        return n / 36;
      },
    );
    expect(await actions.list('a b')).toEqual([info] as never);
    const id = await actions.open('a b', '/w', 0, 5000);
    await actions.resize('a b', id, 120.7, 40);
    await actions.close('a b', id);
    expect(gets).toEqual(['/api/stations/a%20b/shells']);
    expect(posts.map(([p]) => p)).toEqual(Array(3).fill('/api/stations/a%20b/shells'));
    const bodies = posts.map(([, b]) => b as { command_id: string; change: unknown });
    expect(new Set(bodies.map((b) => b.command_id)).size).toBe(3);
    for (const b of bodies) expect(b.command_id).toMatch(/^sh-[0-9a-z]{16}$/);
    expect(bodies.map((b) => b.change)).toEqual([
      { op: 'open', shell_id: id, cwd: '/w', cols: 1, rows: 1000 },
      { op: 'resize', shell_id: id, cols: 120, rows: 40 },
      { op: 'close', shell_id: id },
    ]);
    expect(actions.socketPath('a b', 'shell-x')).toBe('/api/stations/a%20b/shells/shell-x/attach');
  });

  test('a malformed shell list is refused', () => {
    expect(() => shellList({})).toThrow('Invalid shell list');
    expect(() => shellList([{ ...info, state: 'zombie' }])).toThrow('Invalid shell list');
    expect(shellList([{ ...info, state: 'exited', exit_code: 3 }])[0]?.exit_code).toBe(3);
  });

  test('ids and terminal sizes stay inside the station limits', () => {
    expect(newId('shell', () => 0.999)).toBe(`shell-${'z'.repeat(16)}`);
    expect([cells(0), cells(80.9), cells(4000), cells(Number.NaN)]).toEqual([1, 80, 1000, 1]);
  });

  test('handover posts a tui shell for a session and release clears the hold', async () => {
    const posts: [string, unknown][] = [];
    const actions = shellActions(
      async () => [],
      async (path, body) => {
        posts.push([path, body]);
        return { state: 'accepted' };
      },
    );
    const id = await actions.handover('a b', 's-1', 100.5, 0);
    expect(id).toMatch(/^tui-[0-9a-z]{16}$/);
    await actions.release('a b', id);
    expect(posts.map(([p]) => p)).toEqual(Array(2).fill('/api/stations/a%20b/shells'));
    const bodies = posts.map(([, b]) => b as { command_id: string; change: unknown });
    expect(bodies.map((b) => b.change)).toEqual([
      { op: 'handover', shell_id: id, session_id: 's-1', cols: 100, rows: 1 },
      { op: 'release', shell_id: id },
    ]);
  });

  test('a shell list accepts a session id string and refuses any other type', () => {
    expect(shellList([{ ...info, session_id: 's-1' }])[0]?.session_id).toBe('s-1');
    expect(shellList([info])[0]?.session_id).toBeUndefined();
    expect(() => shellList([{ ...info, session_id: 7 }])).toThrow('Invalid shell list');
    expect(() => shellList([{ ...info, session_id: null }])).toThrow('Invalid shell list');
  });
});
