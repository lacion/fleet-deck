// The pure helpers of the spawn form: the directory text, the pins and the workspace request.
// Moved out of `spawn.test.tsx` for the module size rule.
import { expect, test } from 'bun:test';
import { baseProblem, branchProblem, lastParts, togglePin, workspaceRequest } from './spawn';

test('a path shows its last two parts', () => {
  expect(lastParts('/home/me/projects/app')).toBe('projects/app');
  expect(lastParts('/home')).toBe('home');
  expect(lastParts('/')).toBe('/');
  expect(lastParts('/a/b/')).toBe('a/b');
});

test('a pin goes to the front, a second toggle removes it, 20 at most, by station', () => {
  let pins = togglePin({}, 'a', '/x');
  pins = togglePin(pins, 'a', '/y');
  expect(pins['a']).toEqual(['/y', '/x']);
  expect(togglePin(pins, 'a', '/x')['a']).toEqual(['/y']);
  expect(togglePin(pins, 'b', '/z')['b']).toEqual(['/z']);
  let full = {};
  for (let i = 0; i < 25; i += 1) full = togglePin(full, 'a', `/p/${i}`);
  const list = (full as Record<string, string[]>)['a'] ?? [];
  expect(list).toHaveLength(20);
  expect(list[0]).toBe('/p/24');
});

test('branchProblem and baseProblem mirror the station rules (DL-144)', () => {
  expect(branchProblem('feature/PROJ-1')).toBeNull();
  expect(branchProblem('a'.repeat(200))).toBeNull();
  for (const bad of ['-x', 'a b', 'a..b', 'a~1', 'x.lock', 'a/', 'a\tb', 'a:b', 'a[b', '']) {
    expect(branchProblem(bad)).toBe('the branch name is not valid');
  }
  expect(branchProblem('a'.repeat(201))).toBe('the branch name is not valid');
  expect(baseProblem('HEAD~1')).toBeNull();
  expect(baseProblem('origin/main')).toBeNull();
  expect(baseProblem('-x')).toBe('the base is not valid');
  expect(baseProblem('a b')).toBe('the base is not valid');
  expect(baseProblem('')).toBe('the base is not valid');
});

test('workspaceRequest builds the workspace of a start (DL-144)', () => {
  expect(workspaceRequest('here', false, '', '', '')).toBeNull();
  expect(workspaceRequest('here', true, '', 'x', 'y')).toEqual({ kind: 'worktree' });
  expect(workspaceRequest('worktree', false, '', '', '')).toEqual({ kind: 'worktree' });
  expect(workspaceRequest('worktree', false, '', ' feat/x ', ' main ')).toEqual({
    kind: 'worktree',
    branch: 'feat/x',
    base: 'main',
  });
  expect(workspaceRequest('worktree', false, '', 'feat/x', '  ')).toEqual({
    kind: 'worktree',
    branch: 'feat/x',
  });
  // A base needs a branch.
  expect(workspaceRequest('worktree', false, '', '  ', 'main')).toEqual({ kind: 'worktree' });
  // A batch never carries a branch.
  expect(workspaceRequest('worktree', true, '', 'feat/x', 'main')).toEqual({ kind: 'worktree' });
  expect(workspaceRequest('clone', false, ' https://h/o/r.git ', 'x', 'y')).toEqual({
    kind: 'clone',
    url: 'https://h/o/r.git',
  });
});
