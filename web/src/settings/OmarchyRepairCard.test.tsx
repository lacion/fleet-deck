// BG-2: the results of the repair actions on the Omarchy card, the review button, the request
// gate and the one-at-a-time guard.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { DeckError } from './http';
import { type OmarchyActResult, type OmarchyStatus, runAction } from './omarchy';
import { type Gate, gateActions, guarded, OmarchyView, planErrorText } from './OmarchyCard';
import type { RepairsState } from './OmarchyRepairPanels';
import { doneText, type RepairResult, resumeExtra, reviewVisible } from './omarchyRepair';
import { fakeActions, plan, restorePlan, status } from './omarchyFixtures';

const noop = () => {};

describe('the results', () => {
  test('the done texts count the files that the action changed', () => {
    const file = (
      state: RepairResult['files'][number]['state'],
    ): RepairResult['files'][number] => ({
      asset_id: 'fleet_bindings',
      path: '/h/b',
      backup_path: null,
      state,
    });
    const result: RepairResult = {
      code: null,
      repair_id: 'c'.repeat(32),
      files: [file('replaced'), file('replaced'), file('unchanged'), file('restored')],
    };
    expect(doneText('install_replace', result)).toBe(
      'Replaced 2 files. The kept copies are listed under Repairs.',
    );
    expect(doneText('install_resume', result)).toBe('Repair resumed. Replaced 2 files.');
    expect(doneText('restore', result)).toBe('Restored 1 file. The kept copies stay.');
    const one = { ...result, files: [file('replaced')] };
    expect(doneText('install_replace', one)).toBe(
      'Replaced 1 file. The kept copies are listed under Repairs.',
    );
    expect(doneText('install_resume', one)).toBe('Repair resumed. Replaced 1 file.');
    expect(doneText('install_replace', undefined)).toBe(
      'Replaced 0 files. The kept copies are listed under Repairs.',
    );
  });

  const failedWith = (code: string, repair?: RepairResult): OmarchyActResult => ({
    outcome: 'failed',
    code,
    message: 'raw message of the station',
    status: status(),
    ...(repair === undefined ? {} : { repair }),
  });

  test('each coded failure shows its fixed text, with the files', async () => {
    const texts: Record<string, string> = {
      plan_changed: 'The files changed since this review. Review the replacement plan again.',
      unsafe_path: 'Fleet Deck cannot safely replace one of these paths.',
      backup_failed: 'Fleet Deck could not keep a copy of a file. The repair stopped.',
      repair_incomplete:
        'The repair stopped after changing some files. Review the kept copies and repair status.',
      restore_conflict:
        'A current file changed after the repair. Fleet Deck kept both versions and did not overwrite it.',
      build_mismatch: 'This repair was started by another build. Restore the kept copies instead.',
    };
    const repair: RepairResult = {
      code: 'repair_incomplete',
      repair_id: 'c'.repeat(32),
      files: [
        {
          asset_id: 'fleet_bindings',
          path: '/h/fleet-bindings.conf',
          backup_path: '/h/fleet-bindings.conf.kept',
          state: 'backed_up',
        },
      ],
    };
    for (const [code, text] of Object.entries(texts)) {
      const actions = fakeActions({ act: async () => failedWith(code, repair) });
      const end = await runAction(actions, 'a', 'install_replace', async () => 't');
      expect(end).toMatchObject({ kind: 'failed', text, code, repair });
    }
  });

  test('a failed repair shows each file: the state and the kept copy', () => {
    const html = renderToStaticMarkup(
      <OmarchyView
        state={{ kind: 'ready', status: status() }}
        busy={false}
        failure={{
          text: 'The repair stopped after changing some files.',
          message: null,
          lines: [
            {
              line: 'fleet-bindings.conf: Kept aside, not replaced yet',
              kept: 'Kept copy: /h/x.kept',
            },
            { line: 'widget.lua: Not changed', kept: 'No kept copy' },
          ],
        }}
        onAct={noop}
        onRetryRead={noop}
      />,
    );
    expect(html).toContain('fleet-bindings.conf: Kept aside, not replaced yet');
    expect(html).toContain('Kept copy: /h/x.kept');
    expect(html).toContain('widget.lua: Not changed');
    expect(html).toContain('No kept copy');
  });

  test('a done repair has the tone ok', () => {
    const html = renderToStaticMarkup(
      <OmarchyView
        state={{ kind: 'ready', status: status() }}
        busy={false}
        failure={{
          text: 'Replaced 2 files. The kept copies are listed under Repairs.',
          message: null,
          tone: 'ok',
        }}
        onAct={noop}
        onRetryRead={noop}
      />,
    );
    expect(html).toContain('data-tone="ok"');
    expect(html).toContain('Replaced 2 files. The kept copies are listed under Repairs.');
  });

  test('a 409 shows only its fixed text', async () => {
    const actions = fakeActions({
      act: async () => {
        throw new DeckError(409, 'raw text of the deck', 'build_mismatch', {});
      },
    });
    const end = await runAction(actions, 'a', 'install_resume', async () => 't', resumeExtra('x'));
    expect(end).toEqual({
      kind: 'failed',
      text: 'This repair was started by another build. Restore the kept copies instead.',
      message: null,
      status: null,
    });
  });

  test('a failed plan read shows the text of its code, else the plain sentence', () => {
    expect(planErrorText(new DeckError(409, 'raw', 'busy', {}))).toBe(
      'Another Omarchy action is running on this station.',
    );
    expect(planErrorText(new DeckError(501, 'raw', 'unsupported', {}))).toBe(
      "This station's build cannot manage Omarchy from the board.",
    );
    expect(planErrorText(new DeckError(500, 'raw', null, {}))).toBe('The plan could not be read.');
    expect(planErrorText(new Error('x'))).toBe('The plan could not be read.');
  });
});

describe('the review button', () => {
  const card = (s: OmarchyStatus, repairs: RepairsState, refused = false) =>
    renderToStaticMarkup(
      <OmarchyView
        state={{ kind: 'ready', status: s }}
        busy={false}
        failure={null}
        onAct={noop}
        onRetryRead={noop}
        repairs={repairs}
        refused={refused}
      />,
    );
  const read: RepairsState = { kind: 'ready', journals: [] };

  test('shown for changed or missing files', () => {
    expect(card(status({ files_changed: 1 }), read)).toContain(
      '>Review files to replace…</button>',
    );
    expect(card(status({ files_missing: 2 }), read)).toContain(
      '>Review files to replace…</button>',
    );
  });

  test('not shown for a clean install', () => {
    expect(card(status(), read)).not.toContain('Review files to replace…');
  });

  test('shown after a refused install until the next action', () => {
    expect(card(status(), read, true)).toContain('>Review files to replace…</button>');
  });

  test('a failed read of the repairs keeps it; a 501 or no read yet hides it', () => {
    expect(card(status({ files_changed: 1 }), { kind: 'error' })).toContain(
      'Review files to replace…',
    );
    expect(card(status({ files_changed: 1 }), { kind: 'unsupported' })).not.toContain(
      'Review files to replace…',
    );
    expect(card(status({ files_changed: 1 }), { kind: 'idle' })).not.toContain(
      'Review files to replace…',
    );
  });

  test('no review button and no Repairs section on a station without the feature', () => {
    const html = card(status({ files_changed: 1 }), { kind: 'unsupported' }, true);
    expect(html).not.toContain('Review files to replace…');
    expect(html).not.toContain('Repairs');
  });

  test('not shown while an action runs, or without Omarchy', () => {
    expect(reviewVisible({ ...status({ files_changed: 1 }), running: 'install' }, false)).toBe(
      false,
    );
    expect(
      reviewVisible(
        { ...status({ files_changed: 1 }), omarchy: { present: false, version: null } },
        false,
      ),
    ).toBe(false);
    expect(reviewVisible({ ...status(), install: null }, true)).toBe(false);
  });

  test('the Repairs section is on the card once the repairs were read', () => {
    expect(card(status(), read)).toContain('>Repairs</button>');
  });

  test('the buttons are off while an action runs', () => {
    const html = renderToStaticMarkup(
      <OmarchyView
        state={{ kind: 'ready', status: status({ files_changed: 1 }) }}
        busy={true}
        failure={null}
        onAct={noop}
        onRetryRead={noop}
        repairs={read}
      />,
    );
    // The disclosure of Repairs stays on: it only opens a list.
    const action = html.match(/<button(?![^>]*aria-expanded)[^>]*>/g) ?? [];
    expect(action.length).toBeGreaterThan(0);
    for (const button of action) expect(button).toContain('disabled');
  });
});

describe('the gate and the guard', () => {
  test('two clicks do not send two actions', async () => {
    const flag = { current: false };
    let sent = 0;
    let release: () => void = () => undefined;
    const open = new Promise<void>((r) => {
      release = r;
    });
    const click = guarded(flag, async () => {
      sent += 1;
      await open;
    });
    const first = click();
    const second = click();
    release();
    await Promise.all([first, second]);
    expect(sent).toBe(1);
    expect(flag.current).toBe(false);
    await click();
    expect(sent).toBe(2);
  });

  test('the guard opens again after a failed run', async () => {
    const flag = { current: false };
    const click = guarded(flag, async () => {
      throw new Error('boom');
    });
    await expect(click()).rejects.toThrow('boom');
    expect(flag.current).toBe(false);
  });

  test('the new reads go through the gate one after the other', async () => {
    const log: string[] = [];
    let open: () => void = () => undefined;
    const held = new Promise<void>((r) => {
      open = r;
    });
    const raw = fakeActions({
      repairs: async () => {
        log.push('repairs');
        await held;
        return { journals: [] };
      },
      installPlan: async () => {
        log.push('install-plan');
        return plan();
      },
      restorePlan: async (_s, id) => {
        log.push(`restore-plan ${id}`);
        return restorePlan();
      },
      act: async (_s, action) => {
        log.push(`act ${action}`);
        return { outcome: 'done', code: null, message: null, status: status() };
      },
    });
    const gate: Gate = { current: Promise.resolve() };
    const gated = gateActions(raw, gate);
    const a = gated.repairs('alpha');
    const b = gated.installPlan('alpha');
    const c = gated.restorePlan('alpha', 'x');
    const d = gated.act('alpha', 'install_replace', 't');
    await new Promise<void>((r) => setTimeout(r, 0));
    expect(log).toEqual(['repairs']);
    open();
    await Promise.all([a, b, c, d]);
    expect(log).toEqual(['repairs', 'install-plan', 'restore-plan x', 'act install_replace']);
  });
});
