// BG-2: the installer repair on the Omarchy card: the answer validators, the calls, the review
// and restore dialogs, and the Repairs section.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DeckIo } from './http';
import { ELEVATION_HEADER, omarchyActions, runAction } from './omarchy';
import {
  RepairsSection,
  type RepairsState,
  RestoreDialogView,
  ReviewDialogView,
} from './OmarchyRepairPanels';
import {
  type InstallPlanView,
  installPlanOf,
  journalDate,
  newRequestId,
  type OmarchyAssetId,
  type RepairJournalView,
  type RepairResult,
  repairResultOf,
  repairStatusOf,
  replaceExtra,
  restoreExtra,
  type RestorePlanView,
  restorePlanOf,
  resumeExtra,
  selectedEdits,
} from './omarchyRepair';
import { fakeActions, journal, plan, restorePlan, status } from './omarchyFixtures';

const noop = () => {};
const none = new Set<OmarchyAssetId>();

describe('the answer validators', () => {
  test('accept the generated shapes', () => {
    expect(installPlanOf(plan())).toEqual(plan());
    expect(restorePlanOf(restorePlan())).toEqual(restorePlan());
    expect(repairStatusOf({ journals: [journal()] })).toEqual({ journals: [journal()] });
    const result: RepairResult = {
      code: null,
      repair_id: 'c'.repeat(32),
      files: [
        { asset_id: 'fleet_bindings', path: '/h/b', backup_path: '/h/b.kept', state: 'replaced' },
      ],
    };
    expect(repairResultOf(result)).toEqual(result);
  });

  test('a missing kept copy reads as null', () => {
    const got = repairStatusOf({
      journals: [{ ...journal(), files: [{ asset_id: 'fleet_bindings', state: 'published' }] }],
    });
    expect(got.journals[0]?.files[0]?.backup_path).toBeNull();
  });

  test('refuse a broken shape', () => {
    const bad = 'The deck sent an invalid answer.';
    expect(() => installPlanOf(null)).toThrow(bad);
    expect(() => installPlanOf({ ...plan(), plan_id: 7 })).toThrow(bad);
    expect(() => installPlanOf({ ...plan(), files: 'x' })).toThrow(bad);
    expect(() =>
      installPlanOf({ ...plan(), files: [{ ...plan().files[0], classification: 'other' }] }),
    ).toThrow(bad);
    expect(() =>
      installPlanOf({ ...plan(), files: [{ ...plan().files[0], replaceable: 'yes' }] }),
    ).toThrow(bad);
    expect(() =>
      installPlanOf({ ...plan(), files: [{ ...plan().files[0], asset_id: 'fleet_plugin' }] }),
    ).toThrow(bad);
    expect(() => repairStatusOf({ journals: [{ ...journal(), state: 'weird' }] })).toThrow(bad);
    expect(() => repairStatusOf({})).toThrow(bad);
    expect(() =>
      repairStatusOf({ journals: [{ ...journal(), files: [{ asset_id: 'fleet_bindings' }] }] }),
    ).toThrow(bad);
    expect(() =>
      restorePlanOf({ ...restorePlan(), files: [{ ...restorePlan().files[0], action: 'x' }] }),
    ).toThrow(bad);
    expect(() => repairResultOf({ code: null, repair_id: null, files: [{ state: 'x' }] })).toThrow(
      bad,
    );
  });
});

describe('the calls', () => {
  const io = (reply: unknown) => {
    const sent: { method: string; path: string; body?: unknown; headers?: unknown }[] = [];
    const deck: DeckIo = {
      get: async (path, headers) => {
        sent.push({ method: 'GET', path, headers });
        return reply;
      },
      post: async (path, body, headers) => {
        sent.push({ method: 'POST', path, body, headers });
        return reply;
      },
    };
    return { deck, sent };
  };

  test('the three reads use their paths with the encoded names', async () => {
    const a = io(plan());
    await omarchyActions(a.deck).installPlan('my station');
    expect(a.sent[0]?.path).toBe('/api/stations/my%20station/omarchy/install-plan');
    const b = io({ journals: [] });
    await omarchyActions(b.deck).repairs('alpha');
    expect(b.sent[0]?.path).toBe('/api/stations/alpha/omarchy/repairs');
    const c = io(restorePlan());
    await omarchyActions(c.deck).restorePlan('alpha', 'c'.repeat(32));
    expect(c.sent[0]?.path).toBe(
      `/api/stations/alpha/omarchy/repairs/${'c'.repeat(32)}/restore-plan`,
    );
  });

  test('a broken read answer is an error with the fixed text', async () => {
    const a = io({ plan_id: 1 });
    await expect(omarchyActions(a.deck).installPlan('a')).rejects.toThrow(
      'The deck sent an invalid answer.',
    );
  });

  test('act sends the optional fields and the elevation header', async () => {
    const a = io({ outcome: 'done', status: status() });
    const extra = replaceExtra(plan(), ['fleet_bindings'], 'r'.repeat(32));
    await omarchyActions(a.deck).act('alpha', 'install_replace', 'tok', extra);
    expect(a.sent).toEqual([
      {
        method: 'POST',
        path: '/api/stations/alpha/omarchy',
        body: {
          action: 'install_replace',
          plan_id: 'a'.repeat(32),
          replace_recorded_edits: ['fleet_bindings'],
          request_id: 'r'.repeat(32),
        },
        headers: { [ELEVATION_HEADER]: 'tok' },
      },
    ]);
  });

  test('a repair result is validated; a broken one is refused', async () => {
    const good = io({
      outcome: 'failed',
      code: 'backup_failed',
      status: status(),
      repair: { code: 'backup_failed', repair_id: null, files: [] },
    });
    const got = await omarchyActions(good.deck).act('a', 'install_replace', 't');
    expect(got.repair).toEqual({ code: 'backup_failed', repair_id: null, files: [] });
    const bad = io({ outcome: 'done', status: status(), repair: { files: 3 } });
    await expect(omarchyActions(bad.deck).act('a', 'restore', 't')).rejects.toThrow(
      'The deck sent an invalid answer.',
    );
  });

  test('the bodies of resume and restore', () => {
    expect(resumeExtra('c'.repeat(32), 'q')).toEqual({
      repair_id: 'c'.repeat(32),
      request_id: 'q',
    });
    expect(restoreExtra(restorePlan(), ['fleet_bindings'], 'q')).toEqual({
      repair_id: 'c'.repeat(32),
      plan_id: 'b'.repeat(32),
      asset_ids: ['fleet_bindings'],
      request_id: 'q',
    });
  });

  test('each request ID is 32 lowercase hex characters, and new each time', () => {
    const a = newRequestId();
    expect(a).toMatch(/^[0-9a-f]{32}$/);
    expect(newRequestId()).not.toBe(a);
    expect(resumeExtra('x').request_id).toMatch(/^[0-9a-f]{32}$/);
    expect(replaceExtra(plan(), []).request_id).toMatch(/^[0-9a-f]{32}$/);
    expect(restoreExtra(restorePlan(), ['fleet_bindings']).request_id).toMatch(/^[0-9a-f]{32}$/);
  });
});

describe('the review dialog', () => {
  const view = (
    p: InstallPlanView,
    extra: { checked?: Set<OmarchyAssetId>; busy?: boolean } = {},
  ) =>
    renderToStaticMarkup(
      <ReviewDialogView
        load={{ kind: 'ready', plan: p }}
        checked={extra.checked ?? none}
        busy={extra.busy ?? false}
        onToggle={noop}
        onConfirm={noop}
        onCancel={noop}
      />,
    );

  test('the title, the confirmation line and the buttons', () => {
    const html = view(plan());
    expect(html).toContain('aria-label="Review files to replace"');
    expect(html).toContain(
      'Replace the listed integration files? Fleet Deck will keep a separate copy of each current file.',
    );
    expect(html).toContain('>Keep copies and replace</button>');
    expect(html).toContain('>Cancel</button>');
  });

  test('a conflict is listed checked and fixed; a recorded edit is unchecked', () => {
    const html = view(plan());
    expect(html).toContain('/home/o/.config/omarchy/fleet/widget.lua');
    expect(html).toContain('Not recorded by Fleet Deck: replaced, and a copy is kept.');
    expect(html).toContain(
      'You edited this recorded integration file. Fleet Deck will keep it unless you select it.',
    );
    const boxes = html.match(/<input[^>]*>/g) ?? [];
    expect(boxes.length).toBe(2);
    expect(boxes[0]).toContain('checked=""');
    expect(boxes[0]).toContain('disabled=""');
    expect(boxes[1]).not.toContain('checked');
    expect(boxes[1]).not.toContain('disabled');
  });

  test('a selected recorded edit shows checked', () => {
    const html = view(plan(), { checked: new Set<OmarchyAssetId>(['fleet_bindings']) });
    const boxes = html.match(/<input[^>]*>/g) ?? [];
    expect(boxes[1]).toContain('checked=""');
  });

  test('the checkbox is labelled by the displayed path', () => {
    const html = view(plan());
    expect(html).toMatch(
      /<label><input[^>]*\/> <span[^>]*>\/home\/o\/\.config\/hypr\/fleet-bindings\.conf<\/span><\/label>/,
    );
  });

  test('a file that is not replaceable shows its text and disables the confirm button', () => {
    const p = plan();
    const blocked: InstallPlanView = {
      ...p,
      blocked: 'unsafe_path',
      files: [{ ...(p.files[0] as InstallPlanView['files'][number]), replaceable: false }],
    };
    const html = view(blocked);
    expect(html).toContain('Fleet Deck cannot safely replace this path.');
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>Keep copies and replace<\/button>/);
  });

  test('the confirm button is on for a safe plan and off while an action runs', () => {
    expect(view(plan())).not.toMatch(/<button[^>]*disabled=""[^>]*>Keep copies and replace/);
    expect(view(plan(), { busy: true })).toMatch(
      /<button[^>]*disabled=""[^>]*>Keep copies and replace/,
    );
  });

  test('a plan with no file: the text, and the confirm button off', () => {
    const html = view({ ...plan(), files: [] });
    expect(html).toContain('No file needs to be replaced.');
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>Keep copies and replace<\/button>/);
  });

  test('loading and a failed read', () => {
    const loading = renderToStaticMarkup(
      <ReviewDialogView
        load={{ kind: 'loading' }}
        checked={none}
        onToggle={noop}
        onConfirm={noop}
        onCancel={noop}
      />,
    );
    expect(loading).toContain('Loading…');
    expect(loading).toMatch(/<button[^>]*disabled=""[^>]*>Keep copies and replace<\/button>/);
    const failed = renderToStaticMarkup(
      <ReviewDialogView
        load={{ kind: 'error', text: 'The plan could not be read.' }}
        checked={none}
        onToggle={noop}
        onConfirm={noop}
        onCancel={noop}
      />,
    );
    expect(failed).toContain('The plan could not be read.');
  });

  test('only the selected recorded edits go to the request', () => {
    const p = plan();
    expect(selectedEdits(p, none)).toEqual([]);
    expect(
      selectedEdits(p, new Set<OmarchyAssetId>(['fleet_bindings', 'fleet_plugin_widget'])),
    ).toEqual(['fleet_bindings']);
    expect(replaceExtra(p, selectedEdits(p, none), 'q')).toEqual({
      plan_id: 'a'.repeat(32),
      replace_recorded_edits: [],
      request_id: 'q',
    });
  });

  test('the elevation is asked with the label Omarchy integration', async () => {
    const asked: (readonly string[] | undefined)[] = [];
    const sent: unknown[] = [];
    const actions = fakeActions({
      act: async (_s, action, elevation, extra) => {
        sent.push({ action, elevation, extra });
        return { outcome: 'done', code: null, message: null, status: status() };
      },
    });
    await runAction(
      actions,
      'alpha',
      'install_replace',
      async (l) => {
        asked.push(l);
        return 'tok';
      },
      replaceExtra(plan(), ['fleet_bindings'], 'q'),
    );
    expect(asked).toEqual([['Omarchy integration']]);
    expect(sent).toEqual([
      {
        action: 'install_replace',
        elevation: 'tok',
        extra: {
          plan_id: 'a'.repeat(32),
          replace_recorded_edits: ['fleet_bindings'],
          request_id: 'q',
        },
      },
    ]);
  });
});

describe('the Repairs section', () => {
  const ready = (...journals: RepairJournalView[]): RepairsState => ({ kind: 'ready', journals });
  const section = (state: RepairsState, extra: { open?: boolean; working?: boolean } = {}) =>
    renderToStaticMarkup(
      <RepairsSection
        state={state}
        open={extra.open ?? true}
        working={extra.working ?? false}
        onToggle={noop}
        onResume={noop}
        onRestore={noop}
      />,
    );

  test('collapsed by default: the title and no row', () => {
    const html = section(ready(journal()), { open: false });
    expect(html).toContain('>Repairs</button>');
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain('Complete');
  });

  test('the rows: date, state, and each file with its kept copy', () => {
    const html = section(
      ready(
        journal(),
        journal({ repair_id: 'd'.repeat(32), state: 'incomplete' }),
        journal({ repair_id: 'e'.repeat(32), state: 'restored' }),
      ),
    );
    expect(html).toContain('aria-expanded="true"');
    expect(html).toContain(journalDate(1_792_400_000_000));
    expect(html).toContain('<strong>Complete</strong>');
    expect(html).toContain('<strong>Incomplete</strong>');
    expect(html).toContain('<strong>Restored</strong>');
    expect(html).toContain('fleet bindings: Replaced');
    expect(html).toContain('Kept copy: /h/fleet-bindings.conf.kept');
    expect(html).toContain('fleet plugin widget: Not changed');
    expect(html).toContain('No kept copy');
  });

  test('the date is the local date of created_at', () => {
    expect(journalDate(1_792_400_000_000)).toBe(new Date(1_792_400_000_000).toLocaleString());
  });

  test('Resume only for an incomplete journal; Restore for a kept copy', () => {
    const complete = section(ready(journal()));
    expect(complete).not.toContain('Resume repair');
    expect(complete).toContain('>Restore kept copies…</button>');
    const incomplete = section(ready(journal({ state: 'incomplete' })));
    expect(incomplete).toContain('>Resume repair</button>');
    const noCopy = section(
      ready(
        journal({
          files: [{ asset_id: 'fleet_bindings', state: 'nothing_moved', backup_path: null }],
        }),
      ),
    );
    expect(noCopy).not.toContain('Restore kept copies…');
  });

  test('the buttons are off while an action runs', () => {
    const html = section(ready(journal({ state: 'incomplete' })), { working: true });
    expect(
      html.match(/<button[^>]*disabled=""[^>]*>(Resume repair|Restore kept copies…)</g)?.length,
    ).toBe(2);
  });

  test('an empty list, a failed read, no feature, not read yet', () => {
    expect(section(ready())).toContain('No repairs yet.');
    expect(section({ kind: 'error' })).toContain('The repairs could not be read.');
    expect(section({ kind: 'unsupported' })).toBe('');
    expect(section({ kind: 'idle' })).toBe('');
  });

  test('each observed state has its text', () => {
    const states = {
      nothing_moved: 'Not changed',
      backed_up: 'Kept aside, not replaced yet',
      published: 'Replaced',
      restored: 'Restored',
      conflict: 'Changed after the repair',
      incomplete: 'Incomplete',
    } as const;
    for (const [state, text] of Object.entries(states)) {
      const html = section(
        ready(
          journal({
            files: [
              {
                asset_id: 'fleet_bindings',
                state: state as keyof typeof states,
                backup_path: null,
              },
            ],
          }),
        ),
      );
      expect(html).toContain(`fleet bindings: ${text}`);
    }
  });
});

describe('the restore dialog', () => {
  const view = (
    p: RestorePlanView,
    extra: { checked?: Set<OmarchyAssetId>; busy?: boolean } = {},
  ) =>
    renderToStaticMarkup(
      <RestoreDialogView
        load={{ kind: 'ready', plan: p }}
        checked={extra.checked ?? none}
        busy={extra.busy ?? false}
        onToggle={noop}
        onConfirm={noop}
        onCancel={noop}
      />,
    );

  test('the title, the confirmation line and the buttons', () => {
    const html = view(restorePlan());
    expect(html).toContain('aria-label="Restore kept copies"');
    expect(html).toContain(
      'Restore the selected files from their kept copies? A file that changed after the repair is kept aside, never overwritten.',
    );
    expect(html).toContain('>Restore selected</button>');
    expect(html).toContain('>Cancel</button>');
  });

  test('each action has its owner text', () => {
    const html = view(restorePlan());
    expect(html).toContain('Copy the kept file back');
    expect(html).toContain('Keep the current file aside, then copy the kept file back');
    expect(html).toContain('Remove the file that Fleet Deck added');
    expect(html).toContain('theme.tpl: Nothing to restore');
    expect(html).toContain('post-boot: Changed after the repair: not restored');
    expect(html).toContain('theme-set: Incomplete: not restored');
  });

  test('a checkbox only for a file that a restore can change, unchecked at first', () => {
    const html = view(restorePlan());
    const boxes = html.match(/<input[^>]*>/g) ?? [];
    expect(boxes.length).toBe(3);
    for (const box of boxes) expect(box).not.toContain('checked');
    expect(html).toMatch(/<label><input[^>]*\/> <span>widget\.lua<\/span><\/label>/);
    expect(html).not.toContain('<span>theme.tpl</span>');
  });

  test('Restore selected is off until a file is checked', () => {
    expect(view(restorePlan())).toMatch(/<button[^>]*disabled=""[^>]*>Restore selected<\/button>/);
    const on = view(restorePlan(), { checked: new Set<OmarchyAssetId>(['fleet_bindings']) });
    expect(on).not.toMatch(/<button[^>]*disabled=""[^>]*>Restore selected/);
    expect(
      view(restorePlan(), { checked: new Set<OmarchyAssetId>(['fleet_bindings']), busy: true }),
    ).toMatch(/<button[^>]*disabled=""[^>]*>Restore selected/);
  });

  test('no restorable file: the text, and Restore selected off', () => {
    const p = restorePlan();
    const html = view({ ...p, files: p.files.filter((f) => f.action === 'nothing') });
    expect(html).toContain('Nothing can be restored.');
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>Restore selected<\/button>/);
  });
});
