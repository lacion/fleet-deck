// ADR-008 §5.3 "Owner visibility": the fleet-control fact, the dialog and the session action.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { type SessionActions, sessionActions } from './commands';
import { answersText, answersTotal, type FleetAnswers, resultText } from './fleetAnswers';
import { FleetAnswersDialog } from './FleetAnswersDialog';
import { FleetChipButton, useFleetControl } from './FleetControlChip';

const key = { station_id: 'station a', session_id: 's/1' };

const data = (extra: Partial<FleetAnswers> = {}): FleetAnswers => ({
  fleet_control: true,
  answer_cap: 7,
  answer_used: 3,
  last_hour: 2,
  answers: [
    {
      at: 1_700_000_000_000,
      target: 'st-1/w1',
      result: 'ok',
      detail: 'grant=g1',
      command_id: 'c1',
    },
    { at: 1_699_999_000_000, target: null, result: 'refused', detail: null, command_id: null },
    {
      at: 1_699_998_000_000,
      target: 'st-1/w2',
      result: 'failed',
      detail: '<b>x</b>',
      command_id: null,
    },
  ],
  ...extra,
});

/** The fact that the hook gives, rendered as the panel or the menu renders it. */
function Fact({ actions }: { readonly actions: SessionActions | null }) {
  const fleet = useFleetControl(actions);
  return (
    <>
      {fleet.link?.()}
      {fleet.dialog}
    </>
  );
}

describe('fleet control fact', () => {
  test('the value is a link with the answers and those of the last hour, not a filter chip', () => {
    expect(answersText(1, 1)).toBe('1 answer · 1 in the last hour');
    expect(answersText(12, 2)).toBe('12 answers · 2 in the last hour');
    expect(answersText(3, 0)).toBe('3 answers · none in the last hour');
    expect(answersText(1200, 0)).toBe('1,200 answers · none in the last hour');
    // The total: the use of the grant, or the rows of the list, whichever is larger.
    expect(answersTotal(data())).toBe(3);
    expect(answersTotal(data({ answer_used: 12 }))).toBe(12);
    const html = renderToStaticMarkup(
      <FleetChipButton data={data({ last_hour: 1 })} onOpen={() => {}} />,
    );
    expect(html).toContain('class="fd-link fd-props-link fd-fleet-link"');
    expect(html).not.toContain('fd-chip');
    expect(html).toContain('>3 answers · 1 in the last hour</button>');
  });

  test('before the first answer: plain text, no link to an empty list', () => {
    const html = renderToStaticMarkup(
      <FleetChipButton
        data={data({ answers: [], answer_used: 0, last_hour: 0 })}
        onOpen={() => {}}
      />,
    );
    expect(html).toBe('<span class="fd-fleet-none">On · no answers yet</span>');
  });

  test('nothing without a fleet-control grant', () => {
    expect(
      renderToStaticMarkup(
        <FleetChipButton data={data({ fleet_control: false })} onOpen={() => {}} />,
      ),
    ).toBe('');
  });

  test('nothing while loading, and nothing without actions', () => {
    const actions = sessionActions(async () => ({}), key, ['admin']);
    expect(renderToStaticMarkup(<Fact actions={actions} />)).toBe('');
    expect(renderToStaticMarkup(<Fact actions={null} />)).toBe('');
  });
});

describe('fleet answers dialog', () => {
  test('the rows, the limit and the texts of the results', () => {
    const html = renderToStaticMarkup(<FleetAnswersDialog data={data()} onClose={() => {}} />);
    expect(html).toContain('aria-modal="true"');
    expect(html).toContain('Answers of this coordinator');
    expect(html).toContain('3 of 7 used');
    for (const head of ['Time', 'Session', 'Result', 'Details']) {
      expect(html).toContain(`>${head}</th>`);
    }
    expect(html).toContain(new Date(1_700_000_000_000).toLocaleString());
    expect(html).toContain('st-1/w1');
    expect(html).toContain('>—</td>');
    for (const text of ['Answered', 'Refused', 'Failed']) expect(html).toContain(text);
    // The details are text, never markup.
    expect(html).toContain('&lt;b&gt;x&lt;/b&gt;');
    expect(html).not.toContain('<b>x</b>');
    expect(html).toContain('Close');
  });

  test('no rows, and no limit without a cap', () => {
    const html = renderToStaticMarkup(
      <FleetAnswersDialog data={data({ answers: [], answer_cap: null })} onClose={() => {}} />,
    );
    expect(html).toContain('No answers yet.');
    expect(html).not.toContain('Answer limit');
    expect(html).not.toContain('<table');
  });

  test('the result texts', () => {
    expect([resultText('ok'), resultText('refused'), resultText('failed')]).toEqual([
      'Answered',
      'Refused',
      'Failed',
    ]);
  });
});

describe('fleetAnswers session action', () => {
  test('reads the encoded path and checks the shape', async () => {
    const paths: string[] = [];
    const get = async (path: string) => {
      paths.push(path);
      return data();
    };
    const actions = sessionActions(
      async () => ({}),
      key,
      ['admin'],
      () => 'c1',
      get,
    );
    expect(await actions.fleetAnswers()).toEqual(data());
    expect(paths).toEqual(['/api/sessions/station%20a/s%2F1/fleet-answers']);

    const bad = sessionActions(
      async () => ({}),
      key,
      ['admin'],
      () => 'c1',
      async () => ({ fleet_control: true }),
    );
    await expect(bad.fleetAnswers()).rejects.toThrow('The deck sent invalid fleet answers.');
    const none = sessionActions(async () => ({}), key, ['admin']);
    await expect(none.fleetAnswers()).rejects.toThrow('No reader.');
  });
});
