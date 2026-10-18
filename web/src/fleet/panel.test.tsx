// DL-141: the mail form of the fleet panel (static parts; the sends are in the e2e specs).
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { card, station } from '../testing/fleet';
import type { FleetActions } from './fleet';
import { FleetPanel } from './FleetPanel';

const actions: FleetActions = {
  mail: async () => ({ ok: true, text: 'Mail queued.' }),
  task: async () => ({ ok: true, text: 'Task created.' }),
  tasks: async () => [],
  grants: async () => [],
  revoke: async () => ({ ok: true, text: 'Revoked.' }),
  renew: async () => ({ ok: true, text: 'Renewed.' }),
  caps: async () => ({ ok: true, text: 'Limits changed.' }),
  desktopOps: async () => [],
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
});
