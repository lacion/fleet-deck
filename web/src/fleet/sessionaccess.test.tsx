// The "Fleet access…" dialog of the session menu: the grant of one session, and its body.
import { describe, expect, test } from 'bun:test';
import type { ReactElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import type { GrantView } from '../gen';
import { card, station } from '../testing/fleet';
import type { FleetActions } from './fleet';
import { START_LIVE } from './GrantAccess';
import { SessionAccessBody, sessionGrant } from './SessionAccess';

const grant: GrantView = {
  grant_id: 'g',
  station_id: 'station-a',
  session_id: 's1',
  classes: ['read', 'mail', 'drive', 'spawn'],
  stations: ['station-a'],
  spawn_cap: 3,
  spawn_used: 1,
  state: 'active',
  revoked_reason: null,
  generation: 1,
  expires_at: 10,
  last_used_at: null,
  live: true,
};

const session = card('s1', 'working', 'station-a');
const none = {} as FleetActions;

type Body = Parameters<typeof SessionAccessBody>[0];
const body = (extra: Partial<Body> = {}): ReactElement => (
  <SessionAccessBody
    session={session}
    stations={[station('station-a', { coordinator_live: true })]}
    grant={undefined}
    actions={none}
    ask={async () => null}
    fleet={START_LIVE}
    busy={false}
    result={null}
    confirmRevoke={false}
    onFleet={() => {}}
    onGive={() => {}}
    onRevoke={() => {}}
    onConfirmRevoke={() => {}}
    onCancelRevoke={() => {}}
    onRenew={() => {}}
    onChanged={() => {}}
    {...extra}
  />
);
const html = (extra: Partial<Body> = {}) => renderToStaticMarkup(body(extra));

describe('sessionGrant', () => {
  test('finds the grant of the session', () => {
    expect(sessionGrant([grant], session)).toBe(grant);
  });

  test('ignores a revoked grant', () => {
    expect(sessionGrant([{ ...grant, state: 'revoked' }], session)).toBeUndefined();
  });

  test('ignores the grant of another session or station', () => {
    expect(sessionGrant([{ ...grant, session_id: 's2' }], session)).toBeUndefined();
    expect(sessionGrant([{ ...grant, station_id: 'station-b' }], session)).toBeUndefined();
  });

  test('takes the last of two matching grants', () => {
    const newer = { ...grant, grant_id: 'h' };
    expect(sessionGrant([grant, newer], session)).toBe(newer);
  });
});

describe('the dialog body', () => {
  test('a grant: its title, its status, Revoke and Change access', () => {
    const out = html({ grant });
    expect(out).toContain('Coordinator s1');
    expect(out).toContain('active');
    expect(out).toContain('>Revoke<');
    expect(out).toContain('Change access');
    expect(out).not.toContain('Give access');
    expect(out).not.toContain('Revoke fleet access of this session?');
  });

  test('a grant that is not live has no Change access', () => {
    expect(html({ grant: { ...grant, live: false } })).not.toContain('Change access');
  });

  test('Renew shows for a grant that expired from idle only', () => {
    const idle = { ...grant, state: 'expired', revoked_reason: 'idle' };
    expect(html({ grant: idle })).toContain('>Renew<');
    expect(html({ grant: { ...idle, revoked_reason: 'time' } })).not.toContain('>Renew<');
    expect(html({ grant })).not.toContain('>Renew<');
  });

  test('the revoke confirm asks first, with Revoke and Cancel', () => {
    const out = html({ grant, confirmRevoke: true });
    expect(out).toContain('<p class="fd-note">Revoke fleet access of this session?</p>');
    expect(out).toContain('<div class="fd-fleet-actions">');
    expect(out).toContain('class="fd-danger"');
    expect(out).toContain('>Cancel<');
  });

  test('no grant on a live station: the form and Give access', () => {
    const out = html();
    expect(out).toContain('This session has no fleet access.');
    expect(out).toContain('Give access');
    expect(out).toContain('Session limit');
    expect(out).not.toContain('Codex sees the change');
  });

  test('a Codex session gets the Codex note', () => {
    const codex = card('s1', 'working', 'station-a', { provider: 'codex' });
    expect(html({ session: codex })).toContain('Codex sees the change when the session resumes.');
  });

  test('without an elevation, Give access is not shown', () => {
    expect(html({ ask: undefined })).not.toContain('Give access');
  });

  test('a station without coordinator_live gets the refusal text', () => {
    const out = html({ stations: [station('station-a', { coordinator_live: false })] });
    expect(out).toContain('This session cannot get fleet access now');
    expect(out).not.toContain('Give access');
  });

  test('a session that is not running gets the refusal text', () => {
    const out = html({ session: card('s1', 'ended', 'station-a') });
    expect(out).toContain('This session cannot get fleet access now');
    expect(out).not.toContain('Give access');
  });

  test('the load shows a status', () => {
    expect(html({ grant: null })).toContain('<p role="status" class="fd-note">Loading…</p>');
  });

  test('an answer shows as a status, a failed one as an alert', () => {
    expect(html({ result: { ok: true, text: 'Revoked.' } })).toContain(
      '<p class="fd-note" role="status">Revoked.</p>',
    );
    expect(html({ result: { ok: false, text: 'Refused.' } })).toContain(
      '<p class="fd-note" role="alert">Refused.</p>',
    );
  });
});
