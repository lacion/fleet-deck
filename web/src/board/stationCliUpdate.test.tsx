// B28 (design `cli-update.md` §3.12): the provider update lines of the station card.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { CliUpdateReport, StationInfo } from '../gen';
import { attempt, NOW, report, state } from '../settings/cliUpdateFixtures';
import { providerLine } from '../settings/cliUpdate';
import { station } from '../testing/fleet';
import { stationActions } from './pairing';
import { StationCliUpdate } from './StationCliUpdate';
import { StationsPanel } from './StationsPanel';

const line = (extra: Parameters<typeof state>[0], running: string | null = null) =>
  providerLine(state(extra), running, NOW);

describe('the provider line of the station card', () => {
  test('updated by the owner, the schedule or a recovery', () => {
    for (const trigger of ['owner', 'schedule', 'recovery'] as const) {
      expect(line({ last: attempt({ trigger }) })).toEqual({
        text: 'Claude: updated 2.1.308 → 2.1.310 · 3 h ago',
        tone: 'quiet',
      });
    }
  });
  test('updated outside Fleet Deck', () => {
    expect(line({ last: attempt({ trigger: 'external', to_version: '2.1.309' }) })?.text).toBe(
      'Claude: changed outside Fleet Deck, 2.1.308 → 2.1.309 · 3 h ago',
    );
  });
  test('up to date, skipped, refused and verified are quiet', () => {
    expect(line({ last: attempt({ outcome: 'up_to_date' }) })).toEqual({
      text: 'Claude: up to date · checked 3 h ago',
      tone: 'quiet',
    });
    expect(line({ last: attempt({ outcome: 'skipped_busy' }) })?.text).toBe(
      'Claude: not updated, sessions ran · 3 h ago',
    );
    expect(line({ last: attempt({ outcome: 'refused', reason: 'too_new' }) })).toEqual({
      text: 'Claude: not updated, the new version is still in its wait time · 3 h ago',
      tone: 'quiet',
    });
    expect(line({ last: attempt({ outcome: 'verified' }) })?.text).toBe(
      'Claude: checked again, starts allowed · 3 h ago',
    );
  });
  test('an unknown reason says see the station', () => {
    expect(line({ last: attempt({ outcome: 'refused', reason: 'brand_new_code' }) })?.text).toBe(
      'Claude: not updated, see the station · 3 h ago',
    );
    expect(line({ last: attempt({ outcome: 'refused', reason: null }) })?.text).toContain(
      'see the station',
    );
  });
  test('report only and a custom launcher say how to update by hand', () => {
    expect(line({ method: 'homebrew', blocked: 'report_only_method' })).toEqual({
      text: 'Claude: update by hand (Homebrew)',
      tone: 'quiet',
    });
    expect(line({ last: attempt({ outcome: 'report_only' }), method: 'npm' })?.text).toBe(
      'Claude: update by hand (npm)',
    );
    expect(line({ method: 'bun', blocked: 'report_only_method' })?.text).toBe(
      'Claude: update by hand (Bun)',
    );
    expect(line({ blocked: 'custom_launcher' })?.text).toBe(
      'Claude: update by hand (a custom launcher link)',
    );
    expect(line({ method: 'system_package', blocked: 'report_only_method' })?.text).toContain(
      'a system package',
    );
    expect(line({ method: 'unknown', blocked: 'report_only_method' })?.text).toContain(
      'an unknown method',
    );
  });
  test('failed, rolled back and interrupted are hazard', () => {
    expect(line({ last: attempt({ outcome: 'failed', to_version: '2.1.309' }) })).toEqual({
      text: 'Claude: update failed, kept 2.1.308 · 3 h ago',
      tone: 'hazard',
    });
    expect(
      line({
        last: attempt({ outcome: 'rolled_back', trigger: 'recovery', to_version: '2.1.309' }),
      }),
    ).toEqual({
      text: 'Claude: an interrupted update was undone, back on 2.1.309 · 3 h ago',
      tone: 'hazard',
    });
    expect(line({ last: attempt({ outcome: 'rolled_back', to_version: '2.1.309' }) })).toEqual({
      text: 'Claude: 2.1.310 failed the check, back on 2.1.309 · 3 h ago',
      tone: 'hazard',
    });
    expect(line({ last: attempt({ outcome: 'rollback_failed' }) })).toEqual({
      text: 'Claude: update and way back failed. Reinstall Claude on this station.',
      tone: 'hazard',
    });
    expect(line({ last: attempt({ outcome: 'interrupted' }) })).toEqual({
      text: 'Claude: update interrupted · 3 h ago',
      tone: 'hazard',
    });
  });
  test('a running update says where it goes', () => {
    expect(
      line(
        { last: attempt({ attempt_id: 'run1', outcome: 'running', target_version: '2.1.310' }) },
        'run1',
      ),
    ).toEqual({ text: 'Claude: updating to 2.1.310…', tone: 'normal' });
  });
  test('an available version is appended when nothing runs', () => {
    expect(line({ available: '2.1.310', last: attempt({ outcome: 'up_to_date' }) })?.text).toBe(
      'Claude: up to date · checked 3 h ago · 2.1.310 available',
    );
    expect(line({ available: '2.1.310' })?.text).toBe('Claude: 2.1.310 available');
  });
  test('the quarantine wins over the last attempt, the running attempt and the version', () => {
    const last = attempt({ attempt_id: 'run1', outcome: 'updated' });
    expect(line({ quarantined: 'rollback_failed', available: '2.1.310', last }, 'run1')).toEqual({
      text: 'Claude: starts blocked after a failed update. Reinstall Claude, then press Check in Settings.',
      tone: 'hazard',
    });
    expect(line({ quarantined: 'recovery_pending', last })?.text).toBe(
      'Claude: an interrupted update is not finished. Restart the station.',
    );
    expect(line({ quarantined: 'process_left', last })?.text).toBe(
      'Claude: an update process did not stop. Restart the station.',
    );
  });
  test('Codex has its own name', () => {
    expect(
      providerLine(
        state({ provider: 'codex', last: attempt({ outcome: 'up_to_date' }) }),
        null,
        NOW,
      )?.text,
    ).toBe('Codex: up to date · checked 3 h ago');
  });
  test('a provider with nothing to say has no line', () => {
    expect(line({})).toBeNull();
  });
});

const html = (r: CliUpdateReport, canAdmin: boolean) =>
  renderToStaticMarkup(
    <StationCliUpdate report={r} stationId="station-a" canAdmin={canAdmin} now={NOW} />,
  );

describe('the station card block', () => {
  test('the schedule line says off or the window', () => {
    expect(html(report(), false)).toContain('Updates: off');
    expect(html(report({ enabled: true }), false)).toContain('Updates: nightly 03:00-05:00 local');
  });
  test('the Updates link is only for the owner', () => {
    expect(html(report(), true)).toContain('href="#/settings/station/station-a"');
    expect(html(report(), true)).toContain('Updates…');
    expect(html(report(), false)).not.toContain('Updates…');
  });
  test('the hazard tone uses the hazard attribute of the provider line', () => {
    const r = report({ providers: [state({ quarantined: 'rollback_failed' })] });
    expect(html(r, false)).toContain('data-auth="hazard"');
    expect(html(report(), false)).not.toContain('data-auth="hazard"');
  });
});

describe('the station panel', () => {
  const info = (extra: Partial<StationInfo> = {}): StationInfo => ({
    name: 'alpha-host',
    incarnation: 'inc',
    head: 0,
    live_sessions: 0,
    version: null,
    max_sessions: 12,
    allow_shell: null,
    providers: [],
    ...extra,
  });
  const panel = (i: StationInfo) =>
    renderToStaticMarkup(
      <StationsPanel
        stations={[station('station-a', { info: i })]}
        sessions={[]}
        freshness="live"
        actions={stationActions(async () => ({}), true)}
      />,
    );
  test('no line without info.cli_update', () => {
    expect(panel(info())).not.toContain('Updates: ');
    expect(panel(info())).not.toContain('Provider updates');
  });
  test('a station with info.cli_update shows its lines', () => {
    const html = panel(
      info({ cli_update: report({ providers: [state({ quarantined: 'process_left' })] }) }),
    );
    expect(html).toContain('Claude: an update process did not stop. Restart the station.');
    expect(html).toContain('Updates: off');
  });
});
