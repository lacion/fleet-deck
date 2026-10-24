// B18 Phase 2: the Effort field of the spawn form.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { ModelChoice, StationInfo, StationView } from '../gen';
import { claudeCaps } from '../testing/capabilities';
import { card, station } from '../testing/fleet';
import { EffortField, spawnEffortLevels } from './EffortField';
import { SpawnForm } from './SpawnForm';

const choice = (value: string, efforts?: string[], resolved?: string): ModelChoice => ({
  value,
  label: value.toUpperCase(),
  description: null,
  ...(efforts === undefined ? {} : { efforts }),
  ...(resolved === undefined ? {} : { resolved }),
});

const models: ModelChoice[] = [
  choice('opus', ['low', 'medium', 'high', 'max'], 'claude-opus-5-5'),
  choice('sonnet', ['low', 'medium', 'high'], 'claude-sonnet-5-5'),
  choice('haiku', ['low', 'high'], 'claude-haiku-5-5'),
];

describe('spawnEffortLevels', () => {
  test('a chosen model gives its own levels', () => {
    expect(spawnEffortLevels(models, 'opus')).toEqual(['low', 'medium', 'high', 'max']);
    expect(spawnEffortLevels(models, ' sonnet ')).toEqual(['low', 'medium', 'high']);
    expect(spawnEffortLevels(models, 'claude-haiku-5-5')).toEqual(['low', 'high']);
  });

  test('the default model gives the levels that every known model offers', () => {
    expect(spawnEffortLevels(models, '')).toEqual(['low', 'high']);
  });

  test('a model without an advertised list does not narrow the default levels', () => {
    expect(spawnEffortLevels([...models, choice('plain')], '')).toEqual(['low', 'high']);
  });

  test('a model without effort makes the default levels empty', () => {
    expect(spawnEffortLevels([...models, choice('none', [])], '')).toEqual([]);
  });

  test('no list, no known model and no advertised level give no level', () => {
    expect(spawnEffortLevels(null, '')).toEqual([]);
    expect(spawnEffortLevels([], 'opus')).toEqual([]);
    expect(spawnEffortLevels(models, 'gpt-6.1-sol')).toEqual([]);
    expect(spawnEffortLevels([choice('plain')], '')).toEqual([]);
    expect(spawnEffortLevels([choice('plain')], 'plain')).toEqual([]);
  });
});

describe('EffortField', () => {
  const field = (levels: readonly string[], value = '') =>
    renderToStaticMarkup(<EffortField effort={{ levels, value, set: () => {} }} />);

  test('a select named Effort: Default first, then the levels', () => {
    const html = field(['low', 'high'], 'high');
    expect(html).toContain('<select aria-label="Effort" class="fd-spawn-pill"');
    expect(html).toContain('<option value="">Default</option>');
    expect(html).toContain('<option value="low">low</option>');
    expect(html).toContain('<option value="high" selected="">high</option>');
    expect(html.indexOf('>Default<')).toBeLessThan(html.indexOf('>low<'));
  });

  test('Default is selected when no level is chosen', () => {
    expect(field(['low', 'high'])).toContain('<option value="" selected="">Default</option>');
  });

  test('no levels, no field', () => {
    expect(field([])).toBe('');
  });
});

describe('the Effort field in the spawn form', () => {
  const info = (): StationInfo => ({
    name: 'Alpha',
    incarnation: 'inc',
    head: 0,
    live_sessions: 0,
    version: '0.2.0',
    max_sessions: 4,
    allow_shell: null,
    providers: [
      {
        instance: 'claude',
        provider: 'claude',
        version: '2.1.999',
        auth: 'signed_in',
        version_status: 'unknown',
        capabilities: claudeCaps(),
      },
    ],
  });
  const reported = (extra: Partial<StationView> = {}) =>
    station('station-a', { info: info(), info_at: 1, ...extra });
  const known = card('a', 'idle', 'station-a', { provider_instance: 'claude', models });
  const render = (st: StationView, sessions = [known]) =>
    renderToStaticMarkup(
      <SpawnForm stations={[st]} freshness="live" onOpen={() => {}} sessions={sessions} />,
    );

  test('a station with session_effort and known levels shows the field', () => {
    const html = render(reported({ session_effort: true }));
    expect(html).toContain('<select aria-label="Effort" class="fd-spawn-pill"');
    expect(html).toContain('<option value="low">low</option>');
    expect(html).toContain('<option value="high">high</option>');
    expect(html).not.toContain('<option value="max">max</option>');
  });

  test('a station without session_effort hides the field', () => {
    expect(render(reported())).not.toContain('aria-label="Effort"');
    expect(render(reported({ session_effort: false }))).not.toContain('aria-label="Effort"');
  });

  test('no known level hides the field', () => {
    const plain = card('a', 'idle', 'station-a', {
      provider_instance: 'claude',
      models: [choice('opus')],
    });
    expect(render(reported({ session_effort: true }), [plain])).not.toContain(
      'aria-label="Effort"',
    );
    expect(render(reported({ session_effort: true }), [])).not.toContain('aria-label="Effort"');
  });
});
