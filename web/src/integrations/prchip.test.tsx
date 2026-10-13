import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { card } from '../testing/fleet';
import { IntegrationsProvider, type PrMap, readPrs } from './context';
import { fakeActions, prSummary, project } from './fixtures';
import { PrChip } from './PrChip';

const session = card('s1', 'working', 'station-a', { branch: 'feature/1' });

const chip = (pr: Partial<ReturnType<typeof prSummary>>, p = project()) =>
  renderToStaticMarkup(
    <IntegrationsProvider
      actions={fakeActions()}
      initial={[p]}
      initialPrs={new Map([[p.project_id, [prSummary(1, pr)]]])}
    >
      <PrChip card={session} />
    </IntegrationsProvider>,
  );

describe('PrChip (R3.6)', () => {
  test('an open pull request: a link with the number and the state', () => {
    const html = chip({ head_branch: 'feature/1' });
    expect(html).toContain('href="https://github.com/acme/atlas/pull/1"');
    expect(html).toContain('target="_blank"');
    expect(html).toContain('rel="noopener noreferrer"');
    expect(html).toContain('#1 open');
    expect(html).not.toContain('data-tone');
    expect(html).toContain('title="Change 1 · checks passing · review review required"');
  });

  test('a draft shows draft', () => {
    expect(chip({ head_branch: 'feature/1', draft: true })).toContain('#1 draft');
  });

  test('merged is ok and closed is muted', () => {
    expect(chip({ head_branch: 'feature/1', state: 'merged' })).toContain('data-tone="ok"');
    expect(chip({ head_branch: 'feature/1', state: 'merged' })).toContain('#1 merged');
    expect(chip({ head_branch: 'feature/1', state: 'closed' })).toContain('data-tone="muted"');
  });

  test('failing checks on an open pull request make the chip act', () => {
    expect(chip({ head_branch: 'feature/1', checks: 'failing' })).toContain('data-tone="act"');
  });

  test('GitLab uses ! before the number', () => {
    const lab = project({ forge: { kind: 'gitlab', host: 'gitlab.com', repo: 'acme/atlas' } });
    expect(chip({ head_branch: 'feature/1' }, lab)).toContain('!1 open');
  });

  test('without a match nothing is rendered', () => {
    expect(chip({ head_branch: 'other' })).toBe('');
  });
});

describe('readPrs', () => {
  test('reads the projects with a forge only, once each by number', async () => {
    const calls: string[] = [];
    const actions = fakeActions({
      prs: async (id) => {
        calls.push(id);
        const a = prSummary(1);
        return { review_requested: [a], mine: [a], pending: [prSummary(2)], recent_closed: [] };
      },
    });
    const bare = project({ project_id: 'p2', forge: null });
    const map = await readPrs(actions, [project(), bare], new Map());
    expect(calls).toEqual(['p1']);
    expect([...map.keys()]).toEqual(['p1']);
    expect(map.get('p1')?.map((p) => p.number)).toEqual([1, 2]);
  });

  test('a failed read keeps the previous list of that project', async () => {
    const before: PrMap = new Map([['p1', [prSummary(7)]]]);
    const actions = fakeActions({ prs: () => Promise.reject(new Error('down')) });
    const map = await readPrs(actions, [project()], before);
    expect(map.get('p1')?.map((p) => p.number)).toEqual([7]);
  });
});
