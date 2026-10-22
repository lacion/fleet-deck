import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { SettingsEnv } from '../settings/context';
import { Start } from './PairingDialog';

// A tailnet whose device list never answers: the start view shows the count that it is given.
const env = {
  tailnet: { devices: () => new Promise(() => {}) },
} as unknown as SettingsEnv;

/** The `.fd-add-choice` buttons in their order: dim or not, and their inner markup. */
const choices = (html: string) =>
  [
    ...html.matchAll(
      /<button type="button" class="fd-add-choice"( data-dim="true")?>(.*?)<\/button>/g,
    ),
  ].map((m) => ({ dim: m[1] !== undefined, html: m[2] ?? '' }));

describe('Add station start view', () => {
  test('a tailnet without a free computer is a quiet choice after the invitation', () => {
    const html = renderToStaticMarkup(<Start env={env} onPick={() => {}} initialCount={0} />);
    const list = choices(html);
    expect(list).toHaveLength(2);
    expect(list[0]?.html).toContain('Any computer, with a one-time invitation');
    expect(list[0]?.dim).toBe(false);
    expect(list[1]?.html).toContain('A computer on my tailnet');
    expect(list[1]?.dim).toBe(true);
    expect(list[1]?.html).toContain(
      'No computer on your tailnet is free to add. Choose it to see why.',
    );
  });

  test('a tailnet with a free computer comes first, with its count', () => {
    const html = renderToStaticMarkup(<Start env={env} onPick={() => {}} initialCount={2} />);
    const list = choices(html);
    expect(list[0]?.html).toContain('A computer on my tailnet');
    expect(list[0]?.html).toContain('2 computers can be added');
    expect(list[0]?.dim).toBe(false);
    expect(list[1]?.html).toContain('Any computer, with a one-time invitation');
  });

  test('while the count loads, the tailnet choice comes first', () => {
    const html = renderToStaticMarkup(<Start env={env} onPick={() => {}} />);
    const list = choices(html);
    expect(list[0]?.html).toContain('Checking Tailscale…');
    expect(list[0]?.dim).toBe(false);
  });
});
