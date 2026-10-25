// R13 P7b §3.1, §14: the board opt-ins for pointer clicks and the any_window scope.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { FIELDS, meta, view } from '../settings/fixtures';
import { SettingsFields } from '../settings/SettingsFields';
import { FleetControlFields, inputDisclosure } from './FleetControlFields';
import {
  anyAckProblem,
  type FleetControl,
  fleetElevation,
  fleetProblem,
  grantFields,
  inputOn,
  NO_FLEET,
} from './fleetControl';

const INPUT = "Allow keyboard input and pointer movement in this agent's previews";
const CLICK = 'Allow pointer clicks';
const CLICK_DISCLOSURE =
  'Allows this agent to move the pointer and send a left click in its permitted windows. A click can submit forms or approve actions. The pointer or receiving surface can change after Fleet Deck checks it.';
const OWN = "This agent's preview windows";
const ANY = 'Other application windows on the grant stations';
const ACK = 'Allow input in other application windows';
const ANY_DISCLOSURE =
  'This agent can list window positions, view permitted screenshots, and type, press keys and move the pointer in existing applications, including terminals and signed-in browser windows. If clicks are enabled, it can click there too. It can read personal information and act using your logged-in accounts. Fleet Deck refuses detected board and wall windows, but detection and targeting are incomplete. Board sign-out protects this deck only at the machine addresses it records. Agents run as your OS user.';
const HEAD =
  'This agent can type, press keys and move the pointer in preview windows opened for it on the grant stations. Keyboard input can activate buttons and submit forms. Focus can change after Fleet Deck checks it, so input may reach another window, including an owner board window. Board detection is incomplete. Agents run as your OS user.';
const TAIL = 'This feature is experimental until its live smoke passes.';
const BOTH_OFF = 'Pointer clicks and unrestricted window input are unavailable.';
const SCOPE_ONLY = 'Unrestricted window input is unavailable.';
const SIGN_OUT =
  'While an agent types on a station, every board signed in on that station is signed out. Keep this board on another device.';
const ACK_PROBLEM =
  'Check "Allow input in other application windows" to give this agent the broader scope.';

const on: FleetControl = { ...NO_FLEET, desktop: true, capture: true, input: true };
const html = (fleet: FleetControl) =>
  renderToStaticMarkup(<FleetControlFields fleet={fleet} onChange={() => {}} />)
    .replaceAll('&#x27;', "'")
    .replaceAll('&quot;', '"');
const red = (text: string) => `<p class="fd-note fd-fleet-failed">${text}</p>`;
const checked = (kind: 'checkbox' | 'radio', text: string) =>
  new RegExp(`<input type="${kind}"[^>]*checked=""[^>]*/>${text}<`);

describe('grantFields for the click and the scope', () => {
  test('the defaults are off', () => {
    expect(NO_FLEET.click).toBe(false);
    expect(NO_FLEET.anyWindow).toBe(false);
    expect(NO_FLEET.anyAck).toBe(false);
  });

  test('own windows without clicks leaves allow_click out', () => {
    const g = grantFields(on);
    expect(g).toMatchObject({ allow_input: true, input_scope: 'own_windows' });
    expect(g).not.toHaveProperty('allow_click');
  });

  test('own windows with clicks', () => {
    expect(grantFields({ ...on, click: true })).toMatchObject({
      allow_input: true,
      input_scope: 'own_windows',
      allow_click: true,
      input_cap: 100,
      input_character_cap: 20_000,
    });
  });

  test('any_window without clicks leaves allow_click out', () => {
    const g = grantFields({ ...on, anyWindow: true, anyAck: true });
    expect(g).toMatchObject({ allow_input: true, input_scope: 'any_window' });
    expect(g).not.toHaveProperty('allow_click');
  });

  test('any_window with clicks', () => {
    expect(grantFields({ ...on, anyWindow: true, anyAck: true, click: true })).toMatchObject({
      allow_input: true,
      input_scope: 'any_window',
      allow_click: true,
    });
  });

  test('clicks or the scope without the input opt-in give no input field', () => {
    const off = { ...NO_FLEET, desktop: true, capture: true, click: true, anyWindow: true };
    expect(inputOn(off)).toBe(false);
    const g = grantFields(off);
    for (const key of ['allow_input', 'input_scope', 'allow_click', 'input_cap']) {
      expect(g).not.toHaveProperty(key);
    }
  });

  test('the broader scope without the acknowledgment is a problem that blocks the start', () => {
    const bad = { ...on, anyWindow: true };
    expect(anyAckProblem(bad)).toBe(ACK_PROBLEM);
    expect(fleetProblem(bad)).toBe(ACK_PROBLEM);
    expect(anyAckProblem({ ...bad, anyAck: true })).toBeNull();
    expect(fleetProblem({ ...bad, anyAck: true })).toBeNull();
    expect(anyAckProblem(on)).toBeNull();
    // The scope of a switch that is off is not checked.
    expect(anyAckProblem({ ...bad, input: false })).toBeNull();
  });
});

describe('the elevation labels for the click and the scope', () => {
  const labelsOf = async (fleet: FleetControl) => {
    const labels: (readonly string[] | undefined)[] = [];
    await fleetElevation({ coordinator: true, importing: false, fleet }, async (l) => {
      labels.push(l);
      return 'code';
    });
    return labels[0];
  };

  test('each combination', async () => {
    expect(await labelsOf(on)).toEqual(['Fleet control']);
    expect(await labelsOf({ ...on, click: true })).toEqual([
      'Fleet control',
      'Agent pointer clicks',
    ]);
    expect(await labelsOf({ ...on, anyWindow: true, anyAck: true })).toEqual([
      'Fleet control',
      'Agent input in other application windows',
    ]);
    expect(await labelsOf({ ...on, click: true, anyWindow: true, anyAck: true })).toEqual([
      'Fleet control',
      'Agent pointer clicks',
      'Agent input in other application windows',
    ]);
  });

  test('the labels need the input opt-in', async () => {
    const off = { ...NO_FLEET, desktop: true, capture: true, click: true, anyWindow: true };
    expect(await labelsOf(off)).toEqual(['Fleet control']);
  });
});

describe('the form for the click and the scope', () => {
  test('input off shows none of the controls', () => {
    const out = html({ ...NO_FLEET, desktop: true, capture: true });
    for (const hidden of [
      CLICK,
      CLICK_DISCLOSURE,
      'Input windows',
      OWN,
      ANY,
      ACK,
      ANY_DISCLOSURE,
    ]) {
      expect(out).not.toContain(hidden);
    }
  });

  test('input on shows the controls in order with the exact texts, and the defaults', () => {
    const out = html(on);
    const order = [INPUT, CLICK, CLICK_DISCLOSURE, 'Input windows', OWN, ANY];
    const at = order.map((t) => out.indexOf(t));
    expect(at.every((i) => i >= 0)).toBe(true);
    expect([...at].sort((a, b) => a - b)).toEqual(at);
    expect(out).toContain(`<p class="fd-note">${CLICK_DISCLOSURE}</p>`);
    expect(out).toMatch(new RegExp(`<input type="checkbox"[^>]*/>${CLICK}<`));
    expect(out).not.toMatch(checked('checkbox', CLICK));
    expect(out).toMatch(checked('radio', OWN));
    expect(out).not.toMatch(checked('radio', ANY));
    expect(out).not.toContain(ACK);
    expect(out).not.toContain(ANY_DISCLOSURE);
  });

  test('the P7 red disclosure has its three states', () => {
    expect(html(on)).toContain(red(`${HEAD} ${BOTH_OFF} ${TAIL}`));
    expect(html({ ...on, click: true })).toContain(red(`${HEAD} ${SCOPE_ONLY} ${TAIL}`));
    const any = html({ ...on, anyWindow: true, anyAck: true });
    expect(any).toContain(red(`${HEAD} ${TAIL}`));
    expect(any).not.toContain(BOTH_OFF);
    expect(any).not.toContain(SCOPE_ONLY);
    expect(inputDisclosure({ ...on, anyWindow: true, click: true })).toBe(`${HEAD} ${TAIL}`);
    // The sign-out note stays in every state.
    for (const f of [on, { ...on, click: true }, { ...on, anyWindow: true }]) {
      expect(html(f)).toContain(`<p class="fd-note">${SIGN_OUT}</p>`);
    }
  });

  test('the broader red disclosure and the acknowledgment show only with the broader scope', () => {
    expect(html(on)).toContain("Scope: this caller's preview windows");
    expect(html(on)).not.toContain(red(ANY_DISCLOSURE));
    const any = html({ ...on, anyWindow: true });
    expect(any).toContain(red(ANY_DISCLOSURE));
    expect(any).toContain(ACK);
    expect(any).toContain('Scope: other application windows on the grant stations');
    expect(any).not.toContain("Scope: this caller's preview windows");
    expect(any).toMatch(checked('radio', ANY));
    // The acknowledgment is unchecked by default and shows its problem as an alert.
    expect(any).not.toMatch(checked('checkbox', ACK));
    expect(any).toContain(`<p class="fd-note" role="alert">${ACK_PROBLEM}</p>`);
    const acked = html({ ...on, anyWindow: true, anyAck: true });
    expect(acked).toMatch(checked('checkbox', ACK));
    expect(acked).not.toContain(ACK_PROBLEM);
  });

  type Box = { label: string; onChange: (e: { target: { checked: boolean } }) => void };
  const boxesOf = (fleet: FleetControl, calls: FleetControl[]): Box[] => {
    const boxes: Box[] = [];
    const walk = (node: unknown): void => {
      if (Array.isArray(node)) {
        node.forEach(walk);
        return;
      }
      if (typeof node !== 'object' || node === null) return;
      const el = node as { type?: unknown; props?: Record<string, unknown> };
      const kids = el.props?.['children'];
      if (el.type === 'label' && Array.isArray(kids)) {
        const input = kids.find(
          (k): k is { props: { onChange: never } } =>
            typeof k === 'object' && k !== null && (k as { type?: unknown }).type === 'input',
        );
        const text = kids.filter((k) => typeof k === 'string').join('');
        if (input && text) boxes.push({ label: text, onChange: input.props.onChange });
      }
      walk(kids);
    };
    walk(FleetControlFields({ fleet, onChange: (next) => calls.push(next) }));
    return boxes;
  };

  test('the controls change only themselves; input, desktop and capture off turn all three off', () => {
    const calls: FleetControl[] = [];
    const all = { ...on, click: true, anyWindow: true, anyAck: true };
    const boxes = boxesOf(all, calls);
    boxes.find((b) => b.label === CLICK)?.onChange({ target: { checked: false } });
    boxes.find((b) => b.label === ACK)?.onChange({ target: { checked: false } });
    boxes.find((b) => b.label === INPUT)?.onChange({ target: { checked: false } });
    boxes.find((b) => b.label.startsWith('Desktop:'))?.onChange({ target: { checked: false } });
    boxes.find((b) => b.label.startsWith('Screenshots:'))?.onChange({ target: { checked: false } });
    expect(calls[0]).toMatchObject({ click: false, anyWindow: true, anyAck: true, input: true });
    expect(calls[1]).toMatchObject({ click: true, anyWindow: true, anyAck: false });
    const off = { click: false, anyWindow: false, anyAck: false, input: false };
    expect(calls[2]).toMatchObject(off);
    expect(calls[3]).toMatchObject({ ...off, desktop: false });
    expect(calls[4]).toMatchObject({ ...off, capture: false });
  });
});

describe('the station tab shows the two core fields of P7b', () => {
  const fields = [
    ...FIELDS,
    meta('allow_desktop_click', 'Allow agent clicks', 'bool', { danger: true }),
    meta('allow_desktop_input_any', 'Allow agent input in other application windows', 'bool', {
      danger: true,
    }),
  ];
  const out = renderToStaticMarkup(
    <SettingsFields
      view={view({ fields })}
      edits={{}}
      errors={{}}
      onEdit={() => {}}
      stationId="st-alpha"
      savedIds={['claude']}
      secretBlocked={null}
      onSecret={() => {}}
    />,
  );

  test('each shows with its label in the Desktop section, which asks for the password', () => {
    // The board names the second field shorter than the registry (settings/copy.ts).
    expect(out).toContain('Allow agent clicks');
    expect(out).toContain('Agent input in other windows');
    // Both are danger fields, the only fields of the Desktop section: the section says once,
    // next to its heading, that a save asks for the password (no mark on each field).
    const desktop = /<section[^>]*aria-label="Desktop"[\s\S]*?<\/section>/.exec(out)?.[0] ?? '';
    expect(desktop).toContain('Allow agent clicks');
    expect(desktop).toContain('Agent input in other windows');
    expect(desktop).toContain('<span class="fd-set-secnote">');
    expect(desktop.split('Saving asks for your password').length - 1).toBe(1);
    expect(desktop).not.toContain('fd-set-lock');
  });
});
