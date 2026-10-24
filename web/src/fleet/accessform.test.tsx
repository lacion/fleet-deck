// The fields of the fleet access forms: the stations, the permissions and the limits of new sessions.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { NO_FLEET } from '../spawn/fleetControl';
import { station } from '../testing/fleet';
import { AccessFields, AccessStations } from './AccessForm';
import { START_LIVE } from './GrantAccess';

const stations = [
  station('station-a', { coordinator_live: true }),
  station('station-b', { coordinator_live: true }),
];

type Fields = Parameters<typeof AccessFields>[0];
const html = (extra: Partial<Fields> = {}) =>
  renderToStaticMarkup(
    <AccessFields
      stations={stations}
      ownId="station-a"
      picked={[]}
      onPicked={() => {}}
      fleet={NO_FLEET}
      onFleet={() => {}}
      codex={false}
      {...extra}
    />,
  );

describe('the stations', () => {
  test('the own station is on and disabled with its note; the others are off', () => {
    const out = html();
    expect(out).toContain('<h4 class="fd-label fd-access-h">Stations</h4>');
    // One row a line: the title (and its description under it) at the left, the switch at the right.
    expect(out).toContain(
      '<label class="fd-access-row"><span class="fd-access-text"><span class="fd-access-title">Alpha</span><span class="fd-access-desc">The station of this session: always in the access.</span></span><input type="checkbox" role="switch" class="fd-switch" aria-checked="true" disabled="" readOnly="" checked=""/></label>',
    );
    expect(out).toContain(
      '<label class="fd-access-row"><span class="fd-access-text"><span class="fd-access-title">Beta</span></span><input type="checkbox" role="switch" class="fd-switch" aria-checked="false"/></label>',
    );
    expect(out).not.toContain('The stations cannot');
  });

  test('a picked station is on', () => {
    expect(html({ picked: ['station-b'] })).toContain(
      '<span class="fd-access-title">Beta</span></span><input type="checkbox" role="switch" class="fd-switch" aria-checked="true" checked=""/>',
    );
  });

  test('locked: every switch is disabled and the note shows', () => {
    const out = html({ stationsLocked: true, picked: ['station-b'] });
    expect(out).toContain(
      '<span class="fd-access-title">Beta</span></span><input type="checkbox" role="switch" class="fd-switch" aria-checked="true" disabled="" checked=""/>',
    );
    const list = out.slice(0, out.indexOf('Permissions'));
    expect(list).not.toContain(
      '<input type="checkbox" role="switch" class="fd-switch" aria-checked="false"/>',
    );
    expect(out).toContain('To change the stations, revoke the access and give it again.');
  });

  test('AccessStations alone is the same section', () => {
    const out = renderToStaticMarkup(
      <AccessStations stations={stations} ownId="station-b" picked={[]} onPicked={() => {}} />,
    );
    expect(out).toContain(
      '<span class="fd-access-title">Beta</span><span class="fd-access-desc">The station of this session: always in the access.</span>',
    );
    expect(out).toContain('<span class="fd-access-title">Alpha</span>');
  });
});

describe('the permissions', () => {
  test('five rows with a title and a description, in order', () => {
    const out = html();
    expect(out).toContain('<h4 class="fd-label fd-access-h">Permissions</h4>');
    expect(out).toContain(
      'Always: read the fleet, send mail and tasks, and drive the sessions it starts.',
    );
    const rows: readonly (readonly [string, string])[] = [
      ['Start sessions', 'Start new sessions on the picked stations.'],
      [
        'Whole fleet',
        'Reach every session on the picked stations, not only the sessions it starts.',
      ],
      ['Inspect', 'Read diffs, files, artifacts and integrations.'],
      ['Manage', 'Resume, rename, change the mode, and move sessions to a project.'],
      ['Answer approvals', 'Allow once or deny approvals, and answer questions.'],
    ];
    let at = 0;
    for (const [title, desc] of rows) {
      const found = out.indexOf(
        `<span class="fd-access-title">${title}</span><span class="fd-access-desc">${desc}</span>`,
      );
      expect(found).toBeGreaterThan(at);
      at = found;
    }
  });

  test('a switch that is on shows checked', () => {
    expect(html({ fleet: { ...NO_FLEET, inspect: true } })).toContain(
      'integrations.</span></span><input type="checkbox" role="switch" class="fd-switch" aria-checked="true" checked=""/>',
    );
    // No box in a box: the rows have no fill of their own (home.css), and no checkbox look.
    expect(html()).not.toContain('<input type="checkbox"/>');
  });

  test('the edit mode has no Whole fleet row', () => {
    const out = html({ lockedClasses: ['read', 'mail', 'drive'] });
    expect(out).not.toContain('Whole fleet');
    expect(out).toContain('Inspect');
  });
});

describe('the limits section', () => {
  test('it is hidden with nothing that needs a limit', () => {
    expect(html()).not.toContain('Limits');
    expect(html({ fleet: { ...NO_FLEET, inspect: true, scope: true } })).not.toContain('Limits');
  });

  test('spawn shows the three fields and the note', () => {
    const out = html({ fleet: START_LIVE });
    expect(out).toContain('<h4 class="fd-label fd-access-h">Limits</h4>');
    expect(out).toContain('<div class="fd-access-grid">');
    expect(out).toContain('Session limit');
    // No upper bound: the deck checks the configured limit (B27 §3.7).
    expect(out).toContain('min="1" value="3"');
    expect(out).not.toContain('max="50"');
    expect(out).toContain('Highest Claude mode');
    expect(out).toContain('Highest Codex mode');
    // The options say the modes in words; the values stay the ids.
    expect(out).toContain('<option value="plan">Plan</option>');
    expect(out).toContain('<option value="acceptEdits" selected="">Accept edits</option>');
    expect(out).toContain('<option value="never/read-only">Never ask · read only</option>');
    expect(out).toContain(
      '<option value="on-request/workspace-write" selected="">Ask when needed · workspace write</option>',
    );
    expect(out).not.toContain('>acceptEdits<');
    expect(out).toContain('Sessions that it starts, resumes or changes never get a higher mode.');
    expect(out).not.toContain('Answer limit');
  });

  test('manage shows the two modes and no session limit', () => {
    const out = html({ fleet: { ...NO_FLEET, manage: true } });
    expect(out).toContain('Highest Claude mode');
    expect(out).not.toContain('Session limit');
  });

  test('answer alone shows only the answer limit', () => {
    const out = html({ fleet: { ...NO_FLEET, answer: true } });
    expect(out).toContain('Limits');
    expect(out).toContain('Answer limit');
    expect(out).not.toContain('Session limit');
    expect(out).not.toContain('Highest Claude mode');
    expect(out).not.toContain('never get a higher mode');
  });

  test('the limit problem shows under the field', () => {
    const out = html({ fleet: { ...START_LIVE, spawnCap: 'x' } });
    expect(out).toContain('<p class="fd-note" role="alert">The session limit is a whole number');
    expect(html({ fleet: START_LIVE })).not.toContain('role="alert"');
    const answer = html({ fleet: { ...NO_FLEET, answer: true, answerCap: 'x' } });
    expect(answer).toContain('The answer limit is a whole number.');
  });

  test('the edit mode shows a limit only for a newly added class', () => {
    const locked = ['read', 'mail', 'drive', 'spawn'];
    const same = html({ fleet: { ...START_LIVE, answer: true }, lockedClasses: locked });
    expect(same).toContain('Answer limit');
    expect(same).not.toContain('Session limit');
    expect(same).not.toContain('Highest Claude mode');
    const kept = html({ fleet: START_LIVE, lockedClasses: locked });
    expect(kept).not.toContain('Limits');
    const manage = html({ fleet: { ...START_LIVE, manage: true }, lockedClasses: locked });
    expect(manage).toContain('Highest Claude mode');
    expect(manage).not.toContain('Session limit');
  });
});

describe('the Codex note', () => {
  test('it shows for a Codex session only', () => {
    const note = 'Codex sees the change when the session resumes.';
    expect(html({ codex: true })).toContain(note);
    expect(html({ codex: false })).not.toContain(note);
  });
});
