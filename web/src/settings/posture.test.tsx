// B27 §3.7: the posture line and the hazard lines of the settings page.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { FieldMeta, SettingsView } from '../gen';
import type { SettingsActions } from './actions';
import { limitMeta, meta, relaxedItem, view } from './fixtures';
import { readPosture, withPosture } from './flow';
import { PostureLine } from './PostureLine';
import { HAZARDS, hazardText, SettingsFields } from './SettingsFields';

const noop = () => {};
const line = (
  relaxed: Parameters<typeof PostureLine>[0]['relaxed'],
  target: 'station' | 'deck' = 'station',
) => renderToStaticMarkup(<PostureLine relaxed={relaxed} target={target} />);

describe('the posture line', () => {
  test('a station without a posture says so', () => {
    const html = line(undefined);
    expect(html).toContain(
      'This station does not report its security posture. Update the station to see it.',
    );
    expect(html).not.toContain('Relaxed security');
  });

  test('an empty list is calm', () => {
    const html = line([]);
    expect(html).toContain('Security: every control is at its secure default.');
    expect(html).not.toContain('Relaxed security');
    expect(html).not.toContain('Restart the');
  });

  test('a list is a hazard box with the title, the sentence and one item each', () => {
    const html = line([
      relaxedItem('allow_shell', 'Allow terminals'),
      relaxedItem('fleet_summary', 'Share the fleet summary'),
    ]);
    expect(html).toContain('Relaxed security');
    expect(html).toContain('2 controls are changed from their secure default.');
    expect(html).toContain('<li>Allow terminals</li>');
    expect(html).toContain('<li>Share the fleet summary</li>');
    expect(html).toContain('Change a control below to restore its default.');
    expect(html).toContain('data-tone="error"');
  });

  test('one control uses the singular sentence', () => {
    const html = line([relaxedItem('allow_shell', 'Allow terminals')]);
    expect(html).toContain('1 control is changed from its secure default.');
    expect(html).not.toContain('1 controls');
  });

  test('each state has its suffix', () => {
    const html = line([
      relaxedItem('a', 'One', 'active'),
      relaxedItem('b', 'Two', 'after_restart'),
      relaxedItem('c', 'Three', 'until_restart'),
      relaxedItem('d', 'Four', 'command_line'),
    ]);
    expect(html).toContain('<li>One</li>');
    expect(html).toContain('<li>Two (after the next restart)</li>');
    expect(html).toContain('<li>Three (still in effect until the next restart)</li>');
    expect(html).toContain('<li>Four (set on the command line)</li>');
  });

  test('the restart sentence needs an until_restart or after_restart item', () => {
    expect(line([relaxedItem('a', 'One', 'active')])).not.toContain('Restart the');
    expect(line([relaxedItem('a', 'One', 'command_line')])).not.toContain('Restart the');
    expect(line([relaxedItem('a', 'One', 'until_restart')])).toContain(
      'Restart the station to apply the saved values.',
    );
    expect(line([relaxedItem('a', 'One', 'after_restart')], 'deck')).toContain(
      'Restart the deck to apply the saved values.',
    );
  });
});

/** One case per row of the hazard table: [key, a value that shows it, a value that does not]. */
const CASES: readonly [string, unknown, unknown][] = [
  ['allow_bypass_permissions', true, false],
  ['worker_grant.allow_bypass', true, false],
  ['worker_grant.claude_mode', 'bypassPermissions', 'acceptEdits'],
  ['worker_grant.codex_mode', 'never/danger-full-access', 'on-request/workspace-write'],
  ['worker_grant.spawn_cap', 'none', 2],
  ['grant_limits.spawn_cap', 'none', 50],
  ['grant_limits.worker_spawn_cap', 'none', 10],
  ['worker_grant.idle_minutes', 'none', 30],
  ['grant_limits.idle_hours', 'none', 24],
  ['grant_limits.answer_cap', 'none', 5000],
  ['grant_limits.worker_grants', 'none', 200],
  ['default_mode.claude', 'acceptEdits', 'default'],
  ['default_mode.codex', 'never/read-only', 'untrusted/workspace-write'],
];

const TEXTS: Readonly<Record<string, string>> = {
  allow_bypass_permissions: 'Sessions on this station can run every tool without asking.',
  'worker_grant.allow_bypass': 'Workers can start sessions that run every tool without asking.',
  'worker_grant.claude_mode': 'Sessions that a worker starts can skip every permission check.',
  'worker_grant.codex_mode': 'Sessions that a worker starts can skip every permission check.',
  'worker_grant.spawn_cap':
    "No limit: only the grant lifetime and the station's Most sessions stop a runaway agent.",
  'grant_limits.spawn_cap':
    "No limit: only the grant lifetime and the station's Most sessions stop a runaway agent.",
  'grant_limits.worker_spawn_cap':
    "No limit: only the grant lifetime and the station's Most sessions stop a runaway agent.",
  'worker_grant.idle_minutes':
    'No idle limit: a grant stays in effect until its lifetime ends, also when nobody uses it.',
  'grant_limits.idle_hours':
    'No idle limit: a grant stays in effect until its lifetime ends, also when nobody uses it.',
  'grant_limits.answer_cap': 'No limit: an agent can answer every approval while its grant lasts.',
  'grant_limits.worker_grants': 'No limit: every session that you start can get a worker grant.',
  'default_mode.claude':
    'Sessions that start without a mode, also scheduled ones and worker sessions, start in this mode.',
  'default_mode.codex':
    'Sessions that start without a mode, also scheduled ones and worker sessions, start in this mode.',
};

const fieldOf = (key: string): FieldMeta =>
  typeof CASES.find((c) => c[0] === key)?.[1] === 'boolean'
    ? meta(key, key, 'bool')
    : (key.startsWith('worker_grant.') && key.endsWith('_mode')) || key.startsWith('default_mode.')
      ? meta(key, key, 'enum')
      : limitMeta(key, key, 1, 100000, { danger: false });

describe('the hazard lines', () => {
  test('the table has the cases of every row', () => {
    const keys = HAZARDS.flatMap((h) => h.keys).sort();
    expect(keys).toEqual(CASES.map((c) => c[0]).sort());
  });

  for (const [key, on, off] of CASES) {
    test(`${key}`, () => {
      expect(hazardText(key, on)).toBe(TEXTS[key] ?? null);
      expect(hazardText(key, off)).toBeNull();
      const html = (value: unknown) =>
        renderToStaticMarkup(
          <SettingsFields
            view={view({ fields: [fieldOf(key)], values: { [key]: value } })}
            edits={{}}
            errors={{}}
            onEdit={noop}
            stationId={null}
            savedIds={[]}
            secretBlocked={null}
            onSecret={noop}
          />,
        );
      expect(html(on)).toContain('class="fd-set-hazard"');
      expect(html(off)).not.toContain('fd-set-hazard');
    });
  }

  test('the edit decides, not the saved value', () => {
    const f = limitMeta('grant_limits.answer_cap', 'Answers', 1, 100000);
    const html = renderToStaticMarkup(
      <SettingsFields
        view={view({ fields: [f], values: { 'grant_limits.answer_cap': 5000 } })}
        edits={{ 'grant_limits.answer_cap': 'none' }}
        errors={{}}
        onEdit={noop}
        stationId={null}
        savedIds={[]}
        secretBlocked={null}
        onSecret={noop}
      />,
    );
    expect(html).toContain(TEXTS['grant_limits.answer_cap'] ?? '');
  });
});

/** Settings actions whose `view` answers `next` (or fails); the other calls are not used. */
const viewer = (next: () => Promise<SettingsView>): SettingsActions => ({
  view: next,
  save: async () => {
    throw new Error('not used');
  },
  setSecret: async () => {
    throw new Error('not used');
  },
  elevate: async () => {},
  elevation: () => null,
  forgetElevation: () => {},
  restart: async () => {
    throw new Error('not used');
  },
});

// Review finding (B27): a save answer has no posture, so the tab reads the view again after a
// save and replaces only the posture of the view that it shows.
describe('the posture after a save', () => {
  const bypass = relaxedItem('worker_grant.allow_bypass', 'Workers can start bypass sessions');
  const posture = (v: SettingsView) =>
    renderToStaticMarkup(<PostureLine relaxed={v.relaxed} target="deck" />);

  test('a save that relaxes a control shows it without a reload', async () => {
    const shown = view({ relaxed: [] });
    expect(posture(shown)).toContain('Security: every control is at its secure default.');
    const after = withPosture(
      shown,
      await readPosture(
        viewer(async () => view({ relaxed: [bypass], revision: 'rev-2' })),
        { kind: 'deck' },
      ),
    );
    expect(after.relaxed).toEqual([bypass]);
    expect(after.revision).toBe('rev-1');
    expect(posture(after)).toContain('<li>Workers can start bypass sessions</li>');
    expect(posture(after)).not.toContain('every control is at its secure default');
  });

  test('a save that restores a control removes it without a reload', async () => {
    const shown = view({ relaxed: [bypass, relaxedItem('host', 'Host', 'after_restart')] });
    const after = withPosture(
      shown,
      await readPosture(
        viewer(async () => view({ relaxed: [relaxedItem('host', 'Host', 'until_restart')] })),
        { kind: 'deck' },
      ),
    );
    expect(after.relaxed).toEqual([relaxedItem('host', 'Host', 'until_restart')]);
    expect(posture(after)).toContain('<li>Host (still in effect until the next restart)</li>');
    expect(posture(after)).not.toContain('bypass');
    const secure = withPosture(
      after,
      await readPosture(
        viewer(async () => view({ relaxed: [] })),
        { kind: 'deck' },
      ),
    );
    expect(posture(secure)).toContain('Security: every control is at its secure default.');
  });

  test('an older station has no posture after the read', async () => {
    const got = await readPosture(
      viewer(async () => view()),
      { kind: 'station', id: 'st-a' },
    );
    expect(got).toEqual({});
    expect(withPosture(view({ relaxed: [bypass] }), got).relaxed).toBeUndefined();
  });

  test('a failed read keeps the posture that the tab shows', async () => {
    const got = await readPosture(
      viewer(() => Promise.reject(new Error('Failed to fetch'))),
      { kind: 'deck' },
    );
    expect(got).toBeNull();
    const shown = view({ relaxed: [bypass] });
    expect(withPosture(shown, got)).toBe(shown);
  });
});
