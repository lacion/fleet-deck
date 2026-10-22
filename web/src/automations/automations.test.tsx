import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { App } from '../App';
import type { Schedule, ScheduleInput, ScheduleRun, ScheduleView, StationView } from '../gen';
import { station } from '../testing/fleet';
import { AutomationsPage } from './AutomationsPage';
import {
  automationActions,
  browserOffset,
  CUSTOM,
  describeCron,
  describeCronParts,
  draftOf,
  draftProblem,
  emptyDraft,
  inputOf,
  lastRunText,
  presetOf,
  runView,
  scheduleView,
  untilText,
} from './automations';
import { ScheduleForm } from './ScheduleForm';

const NOW = 1_760_000_000_000;

export function schedule(extra: Partial<Schedule> = {}): Schedule {
  return {
    schedule_id: 'sch1',
    name: 'Daily triage',
    enabled: true,
    cron: '@weekdays',
    timezone: '+02:00',
    station_id: 'station-a',
    project_id: null,
    path: '/work/repo',
    provider_instance: 'claude',
    mode: 'plan',
    model: null,
    prompt: 'Triage the <b>new</b> issues.',
    precheck_cmd: null,
    reuse_session: false,
    last_session: null,
    grace_ms: 3_600_000,
    created_at: NOW - 86_400_000,
    ...extra,
  };
}

function run(extra: Partial<ScheduleRun> = {}): ScheduleRun {
  return {
    run_id: 'r1',
    schedule_id: 'sch1',
    due_at: NOW - 3_600_000,
    started_at: NOW - 3_599_000,
    state: 'skipped',
    reason: 'nothing new',
    station_id: 'station-a',
    session_id: null,
    manual: false,
    ...extra,
  };
}

const view = (extra: Partial<ScheduleView> = {}): ScheduleView => ({
  schedule: schedule(),
  next_run_at: NOW + 2 * 3_600_000 + 10 * 60_000,
  last_run: run(),
  ...extra,
});

const linked = (extra: Partial<StationView> = {}): StationView =>
  station('station-a', {
    info: {
      name: 'alpha',
      incarnation: 'inc',
      head: 0,
      live_sessions: 0,
      version: null,
      max_sessions: 4,
      allow_shell: null,
      allow_setup_cmd: true,
      providers: [
        {
          instance: 'claude',
          provider: 'claude',
          version: '2.1.0',
          capabilities: null,
          models: null,
        },
      ],
    } as unknown as StationView['info'],
    ...extra,
  });

describe('automation rules (DL-168)', () => {
  test('presets, custom crons and the words for them', () => {
    expect(presetOf('@daily')).toBe('@daily');
    expect(presetOf(' @hourly ')).toBe('@hourly');
    expect(presetOf('30 2 * * 1-5')).toBe(CUSTOM);
    expect(describeCron('@weekdays')).toBe('Weekdays at 09:00');
    expect(describeCron('30 2 * * 1-5')).toBe('cron 30 2 * * 1-5');
    expect(describeCronParts('@weekdays')).toEqual({ label: 'Weekdays at 09:00' });
    expect(describeCronParts(' 30 2 * * 1-5 ')).toEqual({ cron: '30 2 * * 1-5' });
  });

  test('the browser offset is a fixed offset text', () => {
    expect(browserOffset({ getTimezoneOffset: () => 0 } as Date)).toBe('UTC');
    expect(browserOffset({ getTimezoneOffset: () => -120 } as Date)).toBe('+02:00');
    expect(browserOffset({ getTimezoneOffset: () => 330 } as Date)).toBe('-05:30');
    expect(browserOffset({ getTimezoneOffset: () => -345 } as Date)).toBe('+05:45');
  });

  test('a draft needs the fields that the deck needs', () => {
    const ok = {
      ...emptyDraft('station-a', 'claude'),
      name: 'x',
      path: '/w',
      prompt: 'go',
      timezone: 'UTC',
    };
    expect(draftProblem(ok)).toBeNull();
    expect(draftProblem({ ...ok, name: ' ' })).toContain('name');
    expect(draftProblem({ ...ok, station: '' })).toContain('station');
    expect(draftProblem({ ...ok, path: '' })).toContain('folder');
    expect(draftProblem({ ...ok, instance: '' })).toContain('provider');
    expect(draftProblem({ ...ok, prompt: '' })).toContain('prompt');
    expect(draftProblem({ ...ok, preset: CUSTOM, cron: '' })).toContain('cron');
    expect(draftProblem({ ...ok, timezone: 'Europe/Paris' })).toContain('time zone');
    expect(draftProblem({ ...ok, timezone: '+02:00' })).toBeNull();
    expect(draftProblem({ ...ok, precheck: 'a\0b' })).toContain('NUL');
    expect(draftProblem({ ...ok, graceMinutes: '0' })).toContain('grace');
    expect(draftProblem({ ...ok, graceMinutes: 'x' })).toContain('grace');
  });

  test('the body leaves blank optional fields out and the grace is in milliseconds', () => {
    const d = {
      ...emptyDraft('station-a', 'claude'),
      name: ' Triage ',
      path: ' /w ',
      prompt: 'go',
      timezone: 'UTC',
      preset: CUSTOM,
      cron: ' 30 2 * * 1-5 ',
      graceMinutes: '90',
    };
    const body = inputOf(d);
    expect(body).toEqual({
      name: 'Triage',
      enabled: true,
      cron: '30 2 * * 1-5',
      timezone: 'UTC',
      station: 'station-a',
      path: '/w',
      provider_instance: 'claude',
      prompt: 'go',
      reuse_session: false,
      grace_ms: 5_400_000,
    } satisfies ScheduleInput);
    const full = inputOf({ ...d, mode: 'plan', model: ' opus ', precheck: 'test -s x' });
    expect(full.mode).toBe('plan');
    expect(full.model).toBe('opus');
    expect(full.precheck_cmd).toBe('test -s x');
  });

  test('a schedule becomes a draft and back to the same body', () => {
    const s = schedule({ cron: '15 3 * * *', precheck_cmd: 'true', reuse_session: true });
    const d = draftOf(s);
    expect(d.preset).toBe(CUSTOM);
    expect(d.cron).toBe('15 3 * * *');
    expect(d.graceMinutes).toBe('60');
    const body = inputOf(d);
    expect(body.station).toBe('station-a');
    expect(body.precheck_cmd).toBe('true');
    expect(body.reuse_session).toBe(true);
    expect(body.grace_ms).toBe(3_600_000);
  });

  test('time and last-run texts', () => {
    expect(untilText(NOW + 20_000, NOW)).toBe('due now');
    expect(untilText(NOW + 5 * 60_000, NOW)).toBe('in 5 min');
    expect(untilText(NOW + 2 * 3_600_000, NOW)).toBe('in 2 h');
    expect(untilText(NOW + (2 * 60 + 10) * 60_000, NOW)).toBe('in 2 h 10 min');
    expect(untilText(NOW + 3 * 86_400_000, NOW)).toBe('in 3 days');
    expect(untilText(NOW + 26 * 3_600_000, NOW)).toBe('in 1 day');
    expect(lastRunText(null)).toBe('Never run');
    expect(lastRunText(run())).toBe('Skipped · nothing new');
    expect(lastRunText(run({ state: 'started', reason: '' }))).toBe('Started');
  });
});

describe('automation calls', () => {
  test('the calls use the schedule routes and refuse an invalid answer', async () => {
    const calls: string[] = [];
    const get = async (path: string) => {
      calls.push(`GET ${path}`);
      return path.endsWith('/runs') ? [run()] : [view()];
    };
    const post = async (path: string, body: unknown) => {
      calls.push(`POST ${path} ${JSON.stringify(body).slice(0, 30)}`);
      return path.endsWith('/run') ? run({ manual: true }) : schedule();
    };
    const a = automationActions(get, post);
    expect((await a.list())[0]?.schedule.name).toBe('Daily triage');
    await a.create(inputOf(emptyDraft()));
    await a.update('sch1', inputOf(emptyDraft()));
    await a.remove('sch1');
    expect((await a.runNow('sch1')).manual).toBe(true);
    expect((await a.runs('sch1'))[0]?.state).toBe('skipped');
    expect(calls.map((c) => c.split(' ').slice(0, 2).join(' '))).toEqual([
      'GET /api/schedules',
      'POST /api/schedules',
      'POST /api/schedules/sch1/update',
      'POST /api/schedules/sch1/delete',
      'POST /api/schedules/sch1/run',
      'GET /api/schedules/sch1/runs',
    ]);
    expect(() => scheduleView({ ...schedule(), enabled: 'yes' })).toThrow('invalid automation');
    expect(() => runView({ ...run(), state: 'done' })).toThrow('invalid run');
    const bad = automationActions(async () => ({}), post);
    await expect(bad.list()).rejects.toThrow('invalid list');
    const badRow = automationActions(async () => [{ schedule: schedule() }], post);
    await expect(badRow.list()).rejects.toThrow('invalid automation list');
  });
});

describe('automations page', () => {
  const page = (extra: Partial<React.ComponentProps<typeof AutomationsPage>> = {}) =>
    renderToStaticMarkup(
      <AutomationsPage
        stations={[linked()]}
        actions={automationActions(
          async () => [],
          async () => ({}),
        )}
        live
        now={NOW}
        {...extra}
      />,
    );

  test('a row shows the name, the schedule, the next run and the last run', () => {
    const html = page({ initial: [view()] });
    expect(html).toContain('Daily triage');
    expect(html).toContain('Weekdays at 09:00');
    expect(html).toContain('+02:00');
    expect(html).toContain('Alpha');
    expect(html).toContain('/work/repo');
    expect(html).toContain('in 2 h 10 min');
    expect(html).toContain('Skipped');
    expect(html).toContain('nothing new');
    expect(html).toContain('Run now');
    expect(html).toContain('Run history');
  });

  test('a raw cron expression is code; a preset stays in words', () => {
    const html = page({ initial: [view({ schedule: schedule({ cron: '0 3 * * *' }) })] });
    expect(html).toContain('<span>cron <code>0 3 * * *</code></span>');
    expect(page({ initial: [view()] })).toContain('<span>Weekdays at 09:00</span>');
  });

  test('a disabled automation says so and has no next run', () => {
    const html = page({
      initial: [
        view({ schedule: schedule({ enabled: false }), next_run_at: null, last_run: null }),
      ],
    });
    expect(html).toContain('Off');
    expect(html).toContain('none (off)');
    expect(html).toContain('Turn on');
    expect(html).toContain('never');
  });

  test('text from the deck stays text', () => {
    const html = page({
      initial: [
        view({
          schedule: schedule({ name: '<script>x</script>' }),
          last_run: run({ reason: '<img src=x>' }),
        }),
      ],
    });
    expect(html).toContain('&lt;script&gt;');
    expect(html).toContain('&lt;img src=x&gt;');
    expect(html).not.toContain('<script>');
    expect(html).not.toContain('<img');
  });

  test('without the owner login there are no actions and the page says why', () => {
    const html = page({ actions: undefined });
    expect(html).toContain('need the owner');
    expect(html).toMatch(
      /<button type="button" class="fd-primary" disabled="">[\s\S]*?New automation/,
    );
  });

  test('Run now is disabled with the reason when the station is not linked', () => {
    const html = page({
      stations: [linked({ reachable: false })],
      initial: [view()],
    });
    expect(html).toMatch(/disabled="" title="The station is not linked.">Run now/);
  });

  test('an empty list says what an automation is for', () => {
    expect(page({ initial: [] })).toContain('No automations yet');
  });

  test('the skip rule stays above the list once an automation exists', () => {
    const rule = 'A check on the station can run first; a non-zero exit skips the run.';
    const html = page({ initial: [view()] });
    expect(html).toContain(`<p class="fd-note">${rule}</p>`);
    expect(html.indexOf(rule)).toBeLessThan(html.indexOf('fd-auto-list'));
    expect(page({ initial: [] })).not.toContain(`<p class="fd-note">${rule}</p>`);
  });

  test('the board has an Automations screen and links to it', () => {
    const html = renderToStaticMarkup(
      <App initialRoute={{ name: 'automations' }} stations={[linked()]} freshness="live" />,
    );
    expect(html).toContain('aria-label="Automations"');
    expect(html).toContain('href="#/automations"');
  });
});

describe('automation form', () => {
  const form = (draft = emptyDraft('station-a', 'claude'), st = linked()) =>
    renderToStaticMarkup(
      <ScheduleForm
        stations={[st]}
        initial={draft}
        error={null}
        saving={false}
        editing={false}
        onSave={() => {}}
        onCancel={() => {}}
      />,
    );

  test('the form has the fields of the brief and the presets', () => {
    const html = form();
    for (const text of [
      'Name',
      'Prompt',
      'Station',
      'Repository folder on the station',
      'Provider',
      'Permission mode',
      'Schedule',
      'Time zone',
      'Grace period',
      'Reuse the session of the last run',
      'Run first (optional shell check on the station; a non-zero exit skips the run)',
    ]) {
      expect(html).toContain(text);
    }
    for (const preset of ['@hourly', '@daily', '@weekdays', '@weekly']) {
      expect(html).toContain(preset);
    }
    expect(html).toContain('Create automation');
    // The submit stays enabled: a try shows the errors (DL-171). A fresh form shows none.
    expect(html).toMatch(/<button type="submit" class="fd-primary">Create automation/);
    expect(html).not.toContain('role="alert"');
  });

  test('the cron field shows only for a custom expression', () => {
    expect(form()).not.toContain('Cron (minute hour day month weekday)');
    expect(form({ ...emptyDraft('station-a', 'claude'), preset: CUSTOM })).toContain(
      'Cron (minute hour day month weekday)',
    );
  });

  test('the precheck is disabled with the reason when the station runs no shell commands', () => {
    const off = linked();
    if (off.info !== null) off.info = { ...off.info, allow_setup_cmd: false };
    const html = form(emptyDraft('station-a', 'claude'), off);
    expect(html).toContain('allow_setup_cmd = false');
    expect(html).toMatch(/<textarea[^>]*disabled=""[^>]*><\/textarea>/);
    expect(form()).toContain('Runs with /bin/sh on the station');
  });
});
