import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { App } from '../App';
import type { DeckPlan } from '../gen';
import { station } from '../testing/fleet';
import { PlanLibrary } from './PlanLibrary';
import { planActions, planTitle, planView } from './plans';

export function deckPlan(id: string, extra: Partial<DeckPlan> = {}): DeckPlan {
  return {
    id: `station-a/s1/${id}`,
    station_id: 'station-a',
    session_id: 's1',
    plan_id: id,
    turn_id: 't1',
    session_title: 'Plan the cache',
    markdown: `# Plan ${id}\n1. Read the code\n2. <script>alert(1)</script>`,
    truncated: false,
    status: 'proposed',
    proposed_at: 1_760_000_000_000,
    updated_at: 1_760_000_000_000,
    ...extra,
  };
}

describe('plan library (DL-301)', () => {
  test('the title is the first heading, else the first line, else a placeholder', () => {
    expect(planTitle('intro\n## Steps\n- a')).toBe('Steps');
    expect(planTitle('\n  just text  \nmore')).toBe('just text');
    expect(planTitle('   ')).toBe('Untitled plan');
    expect(planTitle(`# ${'x'.repeat(100)}`)).toHaveLength(80);
  });

  test('the calls use the library routes and refuse an invalid answer', async () => {
    const calls: string[] = [];
    const get = async (path: string) => {
      calls.push(`GET ${path}`);
      return [deckPlan('p1')];
    };
    const post = async (path: string, body: unknown) => {
      calls.push(`POST ${path} ${JSON.stringify(body)}`);
      return { ...deckPlan('p1'), status: 'archived' };
    };
    const a = planActions(get, post, ['read', 'drive']);
    expect(a.canDrive).toBe(true);
    expect((await a.list(false))[0]?.plan_id).toBe('p1');
    await a.list(true);
    expect((await a.mark('station-a/s1/p1', 'archived')).status).toBe('archived');
    expect(calls).toEqual([
      'GET /api/plans',
      'GET /api/plans?archived=true',
      'POST /api/plans/mark {"id":"station-a/s1/p1","status":"archived"}',
    ]);
    expect(planActions(get, post, ['read']).canDrive).toBe(false);
    expect(() => planView({ ...deckPlan('p1'), status: 'done' })).toThrow('invalid plan');
    const bad = planActions(async () => ({}), post, ['read']);
    await expect(bad.list(false)).rejects.toThrow('invalid plan list');
  });

  test('a plan shows its title, status, station and session, and its text stays text', () => {
    const html = renderToStaticMarkup(
      <PlanLibrary
        stations={[station()]}
        actions={undefined}
        live
        initial={[deckPlan('p2', { status: 'executed', truncated: true }), deckPlan('p1')]}
        onOpen={() => {}}
        onExecute={() => {}}
      />,
    );
    expect(html).toContain('Plan p2');
    expect(html).toContain('Executed');
    expect(html).toContain('Alpha');
    expect(html).toContain('Plan the cache');
    expect(html).toContain('&lt;script&gt;');
    expect(html).not.toContain('<script>');
    expect(html).toContain('keeps only the start');
    // Without drive the actions are disabled.
    expect(html).toMatch(
      /<button type="button" class="fd-sm" disabled="">Start a session with this plan/,
    );
  });

  test('a proposed plan offers an assignment only when the board can send mail', () => {
    const render = (onAssign?: () => Promise<{ ok: boolean; text: string }>) =>
      renderToStaticMarkup(
        <PlanLibrary
          stations={[station()]}
          actions={undefined}
          live
          initial={[deckPlan('p1'), deckPlan('p2', { status: 'executed' })]}
          onOpen={() => {}}
          onExecute={() => {}}
          onAssign={onAssign}
        />,
      );
    expect(render()).not.toContain('Assign to a session');
    const html = render(async () => ({ ok: true, text: '' }));
    expect(html.match(/Assign to a session…/g)?.length).toBe(1);
  });

  test('an empty library says how plans arrive, and the board has a Plans screen', () => {
    const html = renderToStaticMarkup(
      <App initialRoute={{ name: 'plans' }} initialPlans={[]} freshness="live" />,
    );
    expect(html).toContain('No plans yet');
    expect(html).toMatch(/aria-current="page"[^>]*>(<svg[\s\S]*?<\/svg>)?Plans</);
  });
});
