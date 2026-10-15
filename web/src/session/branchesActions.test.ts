import { describe, expect, test } from 'bun:test';
import { sessionActions } from './commands';

const key = { station_id: 'st a', session_id: 's/1' };
const diff = {
  session_id: 's/1',
  base_oid: 'a',
  summary: { files_changed: 0, insertions: 0, deletions: 0 },
  patch: '',
  truncated: false,
};

function actions(answer: unknown) {
  const paths: string[] = [];
  const made = sessionActions(
    async () => ({}),
    key,
    ['read'],
    undefined,
    async (path) => {
      paths.push(path);
      return answer;
    },
  );
  return { made, paths };
}

describe('the subagent and branch readers (DL-191)', () => {
  test('there are no readers without a get function', () => {
    const made = sessionActions(async () => ({}), key, ['read']);
    expect(made.subagent).toBeNull();
    expect(made.branches).toBeNull();
    expect(made.branchDiff).toBeNull();
  });

  test('the subagent reader asks the encoded path and checks the shape', async () => {
    const good = { items: [], state: { kind: 'empty' as const } };
    const ok = actions(good);
    expect(await ok.made.subagent?.('toolu/1')).toEqual(good);
    expect(ok.paths).toEqual(['/api/sessions/st%20a/s%2F1/subagents/toolu%2F1']);
    for (const bad of [{}, { items: [] }, { items: [], state: {} }, null]) {
      await expect(actions(bad).made.subagent?.('t')).rejects.toThrow(
        'The deck sent an invalid answer.',
      );
    }
  });

  test('the branches reader asks its path and checks the shape', async () => {
    const good = { head: null, branch: 'main', branches: [], truncated: false };
    const ok = actions(good);
    expect(await ok.made.branches?.()).toEqual(good);
    expect(ok.paths).toEqual(['/api/sessions/st%20a/s%2F1/branches']);
    for (const bad of [{}, { branches: [] }, { branches: {}, truncated: false }, 'x']) {
      await expect(actions(bad).made.branches?.()).rejects.toThrow(
        'The deck sent an invalid answer.',
      );
    }
  });

  test('the branch diff reader asks the encoded head and checks the shape', async () => {
    const ok = actions(diff);
    expect(await ok.made.branchDiff?.('abc/def')).toEqual(diff);
    expect(ok.paths).toEqual(['/api/sessions/st%20a/s%2F1/branches/abc%2Fdef/diff']);
    await expect(actions({ patch: '' }).made.branchDiff?.('h')).rejects.toThrow(
      'The deck sent an invalid answer.',
    );
  });
});
