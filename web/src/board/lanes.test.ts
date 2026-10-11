import { describe, expect, test } from 'bun:test';
import type { Activity } from '../gen';
import { summary } from '../testing/summary';
import { groupByLane, LANES, providerLabel } from './lanes';

describe('lanes', () => {
  test('every activity has exactly one lane, needs-you first', () => {
    const activities: Activity[] = ['idle', 'working', 'needs_you', 'background', 'ended'];
    expect(LANES.map((l) => l.activity).sort()).toEqual([...activities].sort());
    expect(LANES[0]?.activity).toBe('needs_you');
  });

  test('sessions are grouped by activity, newest first', () => {
    const groups = groupByLane([
      summary('a', 'working', 10),
      summary('b', 'needs_you', 5),
      summary('c', 'working', 30),
      summary('d', 'working', 30),
    ]);
    expect(groups.working.map((s) => s.session_id)).toEqual(['c', 'd', 'a']);
    expect(groups.needs_you.map((s) => s.session_id)).toEqual(['b']);
    expect(groups.idle).toEqual([]);
  });

  test('provider labels', () => {
    expect(providerLabel('claude')).toBe('Claude');
    expect(providerLabel('codex')).toBe('Codex');
    expect(providerLabel(null)).toBe('Unknown');
  });
});
