// B18 Phase 2: the TypeScript twins of `fleetdeck_proto::effort` (the same cases as the Rust tests),
// the effort levels of a model, the `modelChoice` of the composer, and the `set_effort` command.
import { describe, expect, test } from 'bun:test';
import type { ModelChoice, SessionSummary } from '../gen';
import { claudeCaps } from '../testing/capabilities';
import { card } from '../testing/fleet';
import { sessionActions } from './commands';
import { type Controller, sessionFlags } from './controller';
import { baseModel, choiceFor, composerModelChoice, effortLevels } from './effortChoice';

const choice = (value: string, resolved?: string, efforts?: string[]): ModelChoice => ({
  value,
  label: value.toUpperCase(),
  description: null,
  ...(resolved === undefined ? {} : { resolved }),
  ...(efforts === undefined ? {} : { efforts }),
});

const FIVE = ['low', 'medium', 'high', 'xhigh', 'max'];
const claudeModels: ModelChoice[] = [
  choice('default', 'claude-opus-5-5', FIVE),
  choice('opus', 'claude-opus-5-5', FIVE),
  choice('sonnet', 'claude-sonnet-5-5', ['low', 'medium', 'high']),
  choice('haiku', 'claude-haiku-5-5', []),
];

describe('baseModel', () => {
  test('removes only a trailing context suffix', () => {
    expect(baseModel('claude-opus-5-5[1m]')).toBe('claude-opus-5-5');
    expect(baseModel('claude-opus-5-5')).toBe('claude-opus-5-5');
    expect(baseModel('opus')).toBe('opus');
    expect(baseModel('[1m]')).toBe('[1m]');
    expect(baseModel('a[1m]b')).toBe('a[1m]b');
  });
});

describe('choiceFor', () => {
  const pickedValue = (model: string) => choiceFor(claudeModels, model)?.value ?? null;

  test('an exact value', () => {
    expect(pickedValue('sonnet')).toBe('sonnet');
    expect(pickedValue('opus')).toBe('opus');
  });

  test('a resolved id', () => {
    expect(pickedValue('claude-haiku-5-5')).toBe('haiku');
  });

  test('the context suffix: the choice with the resolved base id', () => {
    expect(choiceFor(claudeModels, 'claude-opus-5-5[1m]')?.resolved).toBe('claude-opus-5-5');
  });

  test('the first match in list order: default and opus resolve to one id', () => {
    expect(pickedValue('claude-opus-5-5')).toBe('default');
    expect(pickedValue('claude-opus-5-5[1m]')).toBe('default');
  });

  test('no match', () => {
    expect(pickedValue('gpt-6.1-sol')).toBeNull();
    expect(choiceFor([], 'opus')).toBeNull();
  });
});

describe('effortLevels', () => {
  test('the levels of a Claude session whose model is a resolved id', () => {
    expect(effortLevels(claudeModels, 'claude-sonnet-5-5')).toEqual(['low', 'medium', 'high']);
  });

  test('the levels of a [1m] model are those of its base model', () => {
    expect(effortLevels(claudeModels, 'claude-opus-5-5[1m]')).toEqual(FIVE);
  });

  test('no list, no model, no match and a model without effort give no level', () => {
    expect(effortLevels(null, 'claude-sonnet-5-5')).toEqual([]);
    expect(effortLevels(claudeModels, null)).toEqual([]);
    expect(effortLevels(claudeModels, 'gpt-6.1-sol')).toEqual([]);
    expect(effortLevels(claudeModels, 'claude-haiku-5-5')).toEqual([]);
    expect(effortLevels([choice('opus')], 'opus')).toEqual([]);
  });
});

describe('composerModelChoice', () => {
  const key = { station_id: 'station-a', session_id: 's1' };
  const posts: [string, unknown][] = [];
  const actions = sessionActions(
    async (path, body) => {
      posts.push([path, body]);
      return { state: 'done', receipt: { state: 'accepted' } };
    },
    key,
    ['read', 'drive'],
    () => 'c1',
  );
  const acts: string[] = [];
  const controller: Controller = {
    flags: sessionFlags(card('s1'), 'live', actions),
    busy: null,
    outcome: null,
    refusals: {},
    act: (name, call) => {
      acts.push(name);
      void call();
    },
    dismiss: () => {},
  };
  const summaryOf = (set: boolean, extra: Partial<SessionSummary> = {}) =>
    card('s1', 'working', 'station-a', {
      model: 'claude-sonnet-5-5',
      models: claudeModels,
      capabilities: { ...claudeCaps(), set_effort: set },
      ...extra,
    }).summary;

  test('with set_effort and levels, the effort group has the levels and the reported effort', () => {
    const c = composerModelChoice(summaryOf(true, { effort: 'low' }), [], actions, controller);
    expect(c.effort?.levels).toEqual(['low', 'medium', 'high']);
    expect(c.effort?.current).toBe('low');
  });

  test('without set_effort, or without capabilities, there is no effort group', () => {
    expect(composerModelChoice(summaryOf(false), [], actions, controller).effort).toBeUndefined();
    const none = summaryOf(true, { capabilities: null });
    expect(composerModelChoice(none, [], actions, controller).effort).toBeUndefined();
  });

  test('a model without levels has no effort group', () => {
    const haiku = summaryOf(true, { model: 'claude-haiku-5-5' });
    expect(composerModelChoice(haiku, [], actions, controller).effort).toBeUndefined();
  });

  test('a choice sends session.set_effort through the controller', async () => {
    const c = composerModelChoice(summaryOf(true), [], actions, controller);
    c.effort?.onSet('high');
    await Promise.resolve();
    await Promise.resolve();
    expect(acts).toEqual(['effort']);
    expect(posts.map(([p]) => p)).toEqual(['/api/commands/session.set_effort']);
    expect(posts[0]?.[1]).toMatchObject({
      command: { op: 'set_effort', session_id: 's1', effort: 'high' },
    });
  });
});
