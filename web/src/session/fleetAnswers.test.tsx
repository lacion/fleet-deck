// ADR-008 §5.3 "Owner visibility": the chip, the dialog and the session action.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { sessionActions } from './commands';
import { chipText, type FleetAnswers, resultText } from './fleetAnswers';
import { FleetAnswersDialog } from './FleetAnswersDialog';
import { FleetChipButton, FleetControlChip } from './FleetControlChip';

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

describe('fleet control chip', () => {
  test('the text for 0, 1 and 2 answers', () => {
    expect(chipText(0)).toBe('Fleet control · 0 answers in the last hour');
    expect(chipText(1)).toBe('Fleet control · 1 answer in the last hour');
    expect(chipText(2)).toBe('Fleet control · 2 answers in the last hour');
    const html = renderToStaticMarkup(
      <FleetChipButton data={data({ last_hour: 1 })} onOpen={() => {}} />,
    );
    expect(html).toContain('class="fd-chip fd-fleet-chip"');
    expect(html).toContain('Fleet control · 1 answer in the last hour');
  });

  test('nothing without a fleet-control grant', () => {
    expect(
      renderToStaticMarkup(
        <FleetChipButton data={data({ fleet_control: false })} onOpen={() => {}} />,
      ),
    ).toBe('');
  });

  test('nothing while loading', () => {
    const actions = sessionActions(async () => ({}), key, ['admin']);
    expect(renderToStaticMarkup(<FleetControlChip actions={actions} />)).toBe('');
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
