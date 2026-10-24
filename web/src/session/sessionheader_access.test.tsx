// The "Fleet access…" item of the session menu: for an admin owner with the fleet calls only.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { FleetActions } from '../fleet/fleet';
import { ownerEnv, PeopleProvider } from '../sharing/context';
import { card, station } from '../testing/fleet';
import { sessionActions } from './commands';
import { type Controller, sessionFlags } from './controller';
import { SessionHeader } from './SessionHeader';

const s = card('s1', 'working', 'station-a');
const fleet = {} as FleetActions;

describe('the Fleet access menu item', () => {
  const header = (scopes: string[], given: FleetActions | undefined, member = false) => {
    const actions = sessionActions(
      async () => ({}),
      { station_id: 'station-a', session_id: 's1' },
      scopes,
    );
    const idle: Controller = {
      flags: sessionFlags(s, 'live', actions),
      busy: null,
      outcome: null,
      refusals: {},
      act: () => {},
      dismiss: () => {},
    };
    return renderToStaticMarkup(
      <PeopleProvider
        value={
          member
            ? { ...ownerEnv, member: { role: 'collaborator', name: 'Ann', scope: null } }
            : ownerEnv
        }
      >
        <SessionHeader
          session={s}
          stations={[station()]}
          controller={idle}
          actions={actions}
          terminal={null}
          fleet={given}
        />
      </PeopleProvider>,
    );
  };
  const all = ['read', 'drive', 'admin'];

  test('shows for an admin with the fleet calls', () => {
    expect(header(all, fleet)).toContain('Fleet access…');
  });

  test('does not show without the fleet calls', () => {
    expect(header(all, undefined)).not.toContain('Fleet access…');
  });

  test('does not show for a guest', () => {
    expect(header(all, fleet, true)).not.toContain('Fleet access…');
  });

  test('does not show without the admin scope', () => {
    expect(header(['read', 'drive'], fleet)).not.toContain('Fleet access…');
  });
});
