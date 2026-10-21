// The "Fleet access…" dialog of the session menu: the grant of one session, and its body.
import { describe, expect, test } from 'bun:test';
import type { ReactElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import type { GrantView } from '../gen';
import { card, station } from '../testing/fleet';
import type { FleetActions } from './fleet';
import { accessValues, START_LIVE } from './GrantAccess';
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
    picked={[]}
    busy={false}
    result={null}
    confirmRevoke={false}
    editing={false}
    values={accessValues(grant)}
    onFleet={() => {}}
    onPicked={() => {}}
    onValues={() => {}}
    onGive={() => {}}
    onRevoke={() => {}}
    onConfirmRevoke={() => {}}
    onCancelRevoke={() => {}}
    onRenew={() => {}}
    onEdit={() => {}}
    onCancelEdit={() => {}}
    onSave={() => {}}
    onClose={() => {}}
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
  test('the header line says the session title and the station name', () => {
    expect(html({ grant })).toContain('<p class="fd-access-sub">s1 · Alpha</p>');
    const titled = card('s1', 'working', 'station-a', { title: 'Fix the build' });
    expect(html({ session: titled })).toContain('Fix the build · Alpha');
  });

  test('a grant: the summary rows and the buttons', () => {
    const out = html({ grant });
    expect(out).toContain('<dl class="fd-access-sum">');
    expect(out).toContain(
      '<dt>Status</dt><dd>read, mail, drive, spawn, spawned 1 of 3: active</dd>',
    );
    expect(out).toContain(
      '<dt>Permissions</dt><dd><span class="fd-chip">read</span><span class="fd-chip">mail</span>',
    );
    expect(out).toContain('<dt>Stations</dt><dd>Alpha</dd>');
    expect(out).toContain('<dt>Sessions started</dt><dd>1 of 3</dd>');
    expect(out).toContain(`<dt>Expires</dt><dd>${new Date(10).toLocaleString()}</dd>`);
    expect(out).toContain('<div class="fd-access-foot">');
    expect(out).toContain('type="button" class="fd-ghost">Close<');
    expect(out).toContain('>Revoke<');
    expect(out).toContain('>Change permissions<');
    expect(out).not.toContain('>Renew<');
    expect(out).not.toContain('Give access');
    expect(out).not.toContain('Revoke fleet access of this session?');
  });

  test('the stations row says the own station first, and the modes and sessions rows show when set', () => {
    const wide = {
      ...grant,
      stations: ['station-b', 'station-a'],
      max_modes: { claude: 'plan', codex: 'never/read-only' },
    };
    const stations = [
      station('station-a', { coordinator_live: true }),
      station('station-b', { coordinator_live: true }),
    ];
    const out = html({ grant: wide, stations });
    expect(out).toContain('<dt>Stations</dt><dd>Alpha, Beta</dd>');
    expect(out).toContain('<dt>Highest modes</dt><dd>Claude plan · Codex never/read-only</dd>');
    const plain = html({ grant: { ...grant, classes: ['read', 'mail', 'drive'] } });
    expect(plain).not.toContain('Sessions started');
    expect(plain).not.toContain('Highest modes');
  });

  test('a grant that is not live has no Change permissions', () => {
    expect(html({ grant: { ...grant, live: false } })).not.toContain('Change permissions');
  });

  test('without an elevation, a grant has no Change permissions', () => {
    const out = html({ grant, ask: undefined });
    expect(out).not.toContain('Change permissions');
    expect(out).toContain('>Revoke<');
  });

  test('the edit mode: the permissions form with the stations locked, Cancel and Save', () => {
    const out = html({ grant, editing: true });
    expect(out).toContain('aria-label="Change permissions"');
    expect(out).toContain('To change the stations, revoke the access and give it again.');
    expect(out).toContain('Start sessions');
    expect(out).not.toContain('Whole fleet');
    expect(out).not.toContain('Session limit');
    expect(out).not.toContain('<dl');
    expect(out).toContain('type="button" class="fd-ghost">Cancel<');
    expect(out).toContain('type="submit" class="fd-primary">Save<');
    expect(out).not.toContain('Change permissions<');
  });

  test('the edit mode shows the limits of a newly added class only', () => {
    const base = { ...grant, classes: ['read', 'mail', 'drive'] };
    const more = (classes: string[]) =>
      html({ grant: base, editing: true, values: { ...accessValues(base), classes } });
    const answer = more(['read', 'mail', 'drive', 'answer']);
    expect(answer).toContain('Answer limit');
    expect(answer).not.toContain('Highest Claude mode');
    const manage = more(['read', 'mail', 'drive', 'manage']);
    expect(manage).toContain('Highest Claude mode');
    expect(manage).not.toContain('Session limit');
    const spawn = more(['read', 'mail', 'drive', 'spawn']);
    expect(spawn).toContain('Session limit');
    expect(spawn).toContain('Highest Codex mode');
  });

  test('Save is disabled for a limit problem', () => {
    const base = { ...grant, classes: ['read', 'mail', 'drive'] };
    const values = {
      ...accessValues(base),
      classes: ['read', 'mail', 'drive', 'spawn'],
      spawnCap: 'x',
    };
    expect(html({ grant: base, editing: true, values })).toMatch(
      /type="submit" class="fd-primary" disabled="">Save</,
    );
    expect(html({ grant: base, editing: true, busy: true })).toMatch(
      /type="submit" class="fd-primary" disabled="">Save</,
    );
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
    expect(out).not.toContain('fd-access-foot');
  });

  test('no grant on a live station: the form and Give access', () => {
    const out = html();
    expect(out).toContain('This session has no fleet access.');
    expect(out).toContain('Give access');
    expect(out).toContain('Session limit');
    expect(out).not.toContain('Codex sees the change');
    expect(out).toContain('type="submit" class="fd-primary">Give access<');
    expect(out).toContain('type="button" class="fd-ghost">Close<');
    expect(out).toContain('<h4 class="fd-access-h">Stations</h4>');
  });

  test('the give form lists the other stations to pick', () => {
    const stations = [
      station('station-a', { coordinator_live: true }),
      station('station-b', { coordinator_live: true }),
    ];
    const out = html({ stations, picked: ['station-b'] });
    expect(out).toContain('Beta');
    expect(out).toMatch(/<input type="checkbox" checked=""\/><span class="fd-access-title">Beta/);
  });

  test('Give access is disabled for a limit problem', () => {
    expect(html({ fleet: { ...START_LIVE, spawnCap: '99' } })).toMatch(
      /type="submit" class="fd-primary" disabled="">Give access</,
    );
  });

  test('a Codex session gets the Codex note', () => {
    const codex = card('s1', 'working', 'station-a', { provider: 'codex' });
    expect(html({ session: codex })).toContain('Codex sees the change when the session resumes.');
  });

  test('without an elevation, Give access is not shown', () => {
    const out = html({ ask: undefined });
    expect(out).not.toContain('Give access');
    expect(out).toContain('type="button" class="fd-ghost">Close<');
  });

  test('a station without coordinator_live gets the refusal text', () => {
    const out = html({ stations: [station('station-a', { coordinator_live: false })] });
    expect(out).toContain('This session cannot get fleet access now');
    expect(out).not.toContain('Give access');
    expect(out).toContain('type="button" class="fd-ghost">Close<');
  });

  test('a session that is not running gets the refusal text', () => {
    const out = html({ session: card('s1', 'ended', 'station-a') });
    expect(out).toContain('This session cannot get fleet access now');
    expect(out).not.toContain('Give access');
  });

  test('the load shows a status', () => {
    const out = html({ grant: null });
    expect(out).toContain('<p role="status" class="fd-note">Loading…</p>');
    expect(out).toContain('type="button" class="fd-ghost">Close<');
    expect(out).not.toContain('Give access');
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
