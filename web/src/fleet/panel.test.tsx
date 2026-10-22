// DL-141: the mail form of the fleet panel (static parts; the sends are in the e2e specs).
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { card, station } from '../testing/fleet';
import type { FleetActions } from './fleet';
import { FleetPanel, loadLists } from './FleetPanel';

const actions: FleetActions = {
  mail: async () => ({ ok: true, text: 'Mail queued.' }),
  task: async () => ({ ok: true, text: 'Task created.' }),
  tasks: async () => [],
  grants: async () => [],
  revoke: async () => ({ ok: true, text: 'Revoked.' }),
  renew: async () => ({ ok: true, text: 'Renewed.' }),
  caps: async () => ({ ok: true, text: 'Limits changed.' }),
  desktopOps: async () => [],
  give: async () => ({ ok: true, text: '' }),
  changeAccess: async () => ({ ok: true, text: '' }),
};

describe('fleet panel mail form', () => {
  test('the To chips start on One session with the session picker and Send mail', () => {
    const html = renderToStaticMarkup(
      <FleetPanel actions={actions} sessions={[card('a')]} stations={[station()]} />,
    );
    expect(html).toContain('role="group" aria-label="To"');
    for (const label of ['One session', 'Station', 'Repo', 'Everyone'])
      expect(html).toContain(label);
    expect(html).toMatch(/aria-pressed="true">One session</);
    expect(html).toMatch(/aria-pressed="false">Everyone</);
    expect(html).toContain('Session');
    expect(html).toContain('Send mail');
    expect(html).not.toContain('Tasks go to one session');
  });

  test('the section takes a version, and each load reads the tasks and the grants again', async () => {
    let calls = 0;
    const counted: FleetActions = {
      ...actions,
      tasks: async () => {
        calls += 1;
        return [];
      },
    };
    const html = renderToStaticMarkup(
      <FleetPanel
        actions={counted}
        sessions={[card('a')]}
        stations={[station()]}
        layout="section"
        version="1/5"
      />,
    );
    expect(html).toContain('data-layout="section"');
    // The effect runs loadLists once per version (a server render runs no effect).
    const seen: string[] = [];
    loadLists(
      counted,
      () => seen.push('tasks'),
      () => seen.push('grants'),
    );
    loadLists(
      counted,
      () => seen.push('tasks'),
      () => seen.push('grants'),
    );
    await Promise.resolve();
    await Promise.resolve();
    expect(calls).toBe(2);
    expect(seen.filter((x) => x === 'tasks')).toHaveLength(2);
    // A newer load or an unmount drops the late answer of the old one.
    const late: string[] = [];
    const stop = loadLists(
      counted,
      () => late.push('tasks'),
      () => late.push('grants'),
    );
    stop();
    await Promise.resolve();
    await Promise.resolve();
    expect(late).toEqual([]);
  });
});
