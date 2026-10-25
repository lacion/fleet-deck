// B18 Phase 2: the Effort group of the composer model popover, through the full session view.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DeckSession, ModelChoice } from '../gen';
import { claudeCaps } from '../testing/capabilities';
import { card, station } from '../testing/fleet';
import { sessionActions } from './commands';
import { Composer } from './Composer';
import { SessionView } from './SessionView';

const key = { station_id: 'station-a', session_id: 's1' };

const models: ModelChoice[] = [
  {
    value: 'default',
    label: 'Default',
    description: null,
    resolved: 'claude-opus-5-5',
    efforts: ['low', 'medium', 'high', 'xhigh', 'max'],
  },
  {
    value: 'haiku',
    label: 'Haiku',
    description: null,
    resolved: 'claude-haiku-5-5',
    efforts: ['low'],
  },
  { value: 'plain', label: 'Plain', description: null },
];

function session(model: string, effort?: string, setEffort = true): DeckSession {
  return card('s1', 'working', 'station-a', {
    title: 'Fix login',
    capabilities: { ...claudeCaps(), set_effort: setEffort },
    models,
    model,
    ...(effort === undefined ? {} : { effort }),
  });
}

const view = (s: DeckSession) =>
  renderToStaticMarkup(
    <SessionView
      session={s}
      stations={[station()]}
      freshness="live"
      timeline={{
        incarnation: 'inc',
        cursor: 0,
        events: [],
        freshness: 'live',
        error: null,
        ready: true,
      }}
      actions={sessionActions(async () => ({}), key, ['read', 'drive'])}
      layout="desktop"
      tab="chat"
    />,
  );

const rows = (html: string): string[] =>
  [...html.matchAll(/<button type="button" role="radio"[^>]*title="Reasoning effort: (\w+)"/g)].map(
    (m) => m[1] ?? '',
  );

describe('the Effort group of the model popover (B18)', () => {
  test('a Claude session whose model is a resolved id lists the levels of its model', () => {
    const html = view(session('claude-haiku-5-5', 'low'));
    expect(html).toContain('<legend>Effort</legend>');
    expect(rows(html)).toEqual(['low']);
  });

  test('a [1m] model lists the levels of its base model, and the reported level is checked', () => {
    const html = view(session('claude-opus-5-5[1m]', 'xhigh'));
    expect(rows(html)).toEqual(['low', 'medium', 'high', 'xhigh', 'max']);
    expect(html.match(/aria-checked="true"/g)?.length).toBe(1);
    expect(html).toMatch(
      /role="radio" class="fd-mode-row fd-selbar" title="Reasoning effort: xhigh" aria-checked="true" disabled=""/,
    );
  });

  test('without a reported effort no row is checked', () => {
    const html = view(session('claude-opus-5-5[1m]'));
    expect(rows(html).length).toBe(5);
    expect(html).not.toContain('aria-checked="true"');
  });

  test('no group without the set_effort capability, without levels, or for an unknown model', () => {
    expect(view(session('claude-opus-5-5[1m]', 'high', false))).not.toContain('<legend>Effort');
    expect(view(session('plain'))).not.toContain('<legend>Effort');
    expect(view(session('gpt-6.1-sol'))).not.toContain('<legend>Effort');
  });

  test('the composer without an effort choice has no group', () => {
    const html = renderToStaticMarkup(
      <Composer
        enabled
        working={false}
        steer={false}
        nativeQueue={false}
        onSend={() => {}}
        model="Opus 5.5"
        modelChoice={{ current: 'opus', models, onSet: () => {} }}
      />,
    );
    expect(html).toContain('<legend>Model</legend>');
    expect(html).not.toContain('<legend>Effort');
  });

  test('the group sits under the models in the same popover', () => {
    const html = view(session('claude-opus-5-5[1m]', 'low'));
    expect(html.indexOf('<legend>Model</legend>')).toBeLessThan(
      html.indexOf('<legend>Effort</legend>'),
    );
    expect(html.match(/<legend>Effort<\/legend>/g)?.length).toBe(1);
  });
});
