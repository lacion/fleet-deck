import { describe, expect, test } from 'bun:test';
import type { WorkspaceListing } from '../gen';
import { spawnActions } from './spawn';

const listing: WorkspaceListing = {
  path: '/home/me/projects',
  parent: null,
  entries: [],
  truncated: false,
};

describe('the import pin (B7)', () => {
  test('an import carries the pin of a verified entry ; a plain start never does', async () => {
    const posts: { path: string; body: unknown }[] = [];
    const actions = spawnActions(
      async () => listing,
      async (path, body) => {
        posts.push({ path, body });
        return {
          state: 'done',
          receipt: { state: 'accepted', result: { session_id: 's-pin' }, reject: null },
        };
      },
      ['read', 'drive'],
      () => 'b-pin',
    );
    const spec = {
      provider_instance: 'claude',
      cwd: '/home/me/projects/app',
      mode: null,
      model: null,
      title: null,
    };
    const pin = { home: '/home/me/.claude', dev: 7, ino: 99 };
    await actions.start('st', spec, '', null, 'abc', undefined, undefined, undefined, pin);
    await actions.start('st', spec, '', null, 'abc');
    await actions.start('st', spec, 'go', null, undefined, undefined, undefined, undefined, pin);
    const commands = posts.map((p) => (p.body as { command: Record<string, unknown> }).command);
    expect(commands[0]).toEqual({
      op: 'import',
      spec,
      native_session_id: 'abc',
      prompt: null,
      transcript: pin,
    });
    expect(commands[1]).not.toHaveProperty('transcript');
    expect(commands[2]).not.toHaveProperty('transcript');
  });
});
