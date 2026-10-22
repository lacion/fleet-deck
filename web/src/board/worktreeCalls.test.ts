// DL-142: the worktree inventory calls of a station (moved out of fleet.test.tsx for the module
// size rule).
import { describe, expect, test } from 'bun:test';
import type { WorktreeInfo } from '../gen';
import { removeCommand, worktreeLoader } from './pairing';

describe('worktree inventory calls (DL-142)', () => {
  const worktree = (extra: Partial<WorktreeInfo> = {}): WorktreeInfo => ({
    session_id: 's1',
    title: null,
    path: '/w/deck-wt/s1',
    branch: 'main-fix',
    main: '/w/deck',
    exists: true,
    registered: true,
    dirty: 0,
    ignored: 0,
    unpushed: 0,
    compared_with: 'remotes',
    last_commit: null,
    live: false,
    verdict: 'clean',
    note: null,
    ...extra,
  });

  test('the remove command quotes the values and deletes only an fd/ branch', () => {
    expect(removeCommand(worktree())).toBe("git -C '/w/deck' worktree remove '/w/deck-wt/s1'");
    expect(removeCommand(worktree({ branch: 'fd/s1' }))).toBe(
      "git -C '/w/deck' worktree remove '/w/deck-wt/s1' && git -C '/w/deck' branch -d 'fd/s1'",
    );
    expect(removeCommand(worktree({ path: "/w/it's here", main: "/w/o'k" }))).toBe(
      "git -C '/w/o'\\''k' worktree remove '/w/it'\\''s here'",
    );
  });

  test('there is no command for a missing, live or main-less worktree', () => {
    expect(removeCommand(worktree({ exists: false }))).toBeNull();
    expect(removeCommand(worktree({ live: true }))).toBeNull();
    expect(removeCommand(worktree({ main: null }))).toBeNull();
  });

  test('the loader reads the station route and checks the list', async () => {
    const paths: string[] = [];
    const load = worktreeLoader(async (path) => {
      paths.push(path);
      return path.includes('bad') ? { worktrees: 1 } : { worktrees: [worktree()] };
    });
    expect((await load('a b')).worktrees).toHaveLength(1);
    expect(paths).toEqual(['/api/stations/a%20b/worktrees']);
    await expect(load('bad')).rejects.toThrow('The deck sent an invalid worktree list.');
  });
});
