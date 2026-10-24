// DL-140: the approval dock of a session (moved out of controls.test.tsx, which passed 800 lines).
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { OpenRequest } from '../gen';
import { ApprovalDock } from './ApprovalDock';

function request(id: string, extra: Partial<OpenRequest> = {}): OpenRequest {
  return {
    request_id: id,
    node_id: null,
    request_kind: 'tool',
    title: `Run ${id}`,
    details: 'rm -rf dist',
    input: null,
    options: [
      { option_id: 'allow', label: 'Allow', kind: 'allow_once' },
      { option_id: 'deny', label: 'Deny', kind: 'deny' },
    ],
    response: 'rpc_reply',
    requested_at: 1,
    ...extra,
  };
}

describe('approval dock (DL-140)', () => {
  const dock = (
    requests: OpenRequest[],
    answerable = true,
    enabled = true,
    from?: ReadonlyMap<string, string | null>,
  ) =>
    renderToStaticMarkup(
      <ApprovalDock
        requests={requests}
        answerable={answerable}
        enabled={enabled}
        busy={false}
        from={from}
        onAnswer={() => {}}
      />,
    );

  test('no request renders nothing', () => {
    expect(dock([])).toBe('');
  });

  test('two requests show the count and one primary option each', () => {
    const html = dock([request('r1'), request('r2')]);
    expect(html).toContain('Needs you · 2');
    expect(html).toContain('aria-label="Needs you"');
    expect(html.match(/class="fd-decide"/g)).toHaveLength(2);
    expect(html).toContain('Deny');
  });

  test('a deny option is a danger button', () => {
    expect(dock([request('r1')])).toContain(
      '<button type="button" class="fd-danger">Deny</button>',
    );
  });

  test('a permission request names its kind and its age', () => {
    const html = dock([request('r1', { requested_at: Date.now() - 5 * 60_000 })]);
    expect(html).toContain('Permission request');
    expect(html).toContain('<span class="fd-q-dock-age">5 min</span>');
  });

  test('the reason line shows the text of input.description, and only a text', () => {
    const reason = dock([request('r1', { input: { description: 'Runs the test suite' } })]);
    expect(reason).toContain('<p class="fd-q-dock-reason">Runs the test suite</p>');
    for (const input of [{ description: '' }, { description: '   ' }, 'Runs', ['Runs'], null]) {
      expect(dock([request('r1', { input })])).not.toContain('fd-q-dock-reason');
    }
  });

  test('without the approvals capability or the scope it says why', () => {
    expect(dock([request('r1')], false)).toContain('This provider has no approval controls.');
    expect(dock([request('r1')], true, false)).toContain('Answering needs the approve scope.');
  });

  test('an approval of a workflow agent says From workflow agent <label>', () => {
    const from = new Map<string, string | null>([
      ['r1', 'Read files'],
      ['r2', null],
    ]);
    const html = dock([request('r1'), request('r2'), request('r3')], true, true, from);
    expect(html).toContain('<p class="fd-note">From workflow agent Read files</p>');
    expect(html).toContain('<p class="fd-note">From a workflow agent</p>');
    expect(html.match(/From (a )?workflow agent/g)).toHaveLength(2);
    expect(dock([request('r1')])).not.toContain('From workflow agent');
  });

  test('a refusal is shown under its request only (G4)', () => {
    const html = renderToStaticMarkup(
      <ApprovalDock
        requests={[request('r1'), request('r2')]}
        answerable
        enabled
        busy={false}
        refusals={{ r2: 'Refused: no such request (2 times)' }}
        onAnswer={() => {}}
      />,
    );
    expect(html.match(/role="alert"/g)).toHaveLength(1);
    const items = html.split('<li').slice(1);
    const [first, second] = items;
    expect(first).not.toContain('role="alert"');
    expect(second).toContain('data-request="r2"');
    expect(second).toContain('Refused: no such request (2 times)');
  });
});
