import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DesktopActBody, DesktopInfo, PreviewWindow } from '../gen';
import type { DesktopActions } from '../displays/actions';
import { SettingsProvider, type SettingsEnv } from '../settings/context';
import { station } from '../testing/fleet';
import { AgentPreviews, closePreview, previewText } from './AgentPreviews';
import { DesktopPanel } from './DesktopPanel';

const noop = () => {};

const preview = (over: Partial<PreviewWindow> = {}): PreviewWindow => ({
  window_id: 'ab12',
  monitor: null,
  caller_label: 'bold-fox',
  ...over,
});

const info = (over: Partial<DesktopInfo> = {}): DesktopInfo => ({
  kind: 'omarchy',
  omarchy_version: '3.1',
  monitors: [],
  locked: false,
  dnd: false,
  idle_inhibited: false,
  walls: [],
  allowed: { desktop: true, capture: false, capture_monitors: [], input: false },
  tools: {
    wtype: true,
    pointer: false,
    recorder: false,
    grim: true,
    chromium: true,
    notify_send: true,
  },
  features: [],
  ...over,
});

describe('the agent previews section', () => {
  test('no previews, or an absent field: nothing is rendered', () => {
    expect(
      renderToStaticMarkup(<AgentPreviews previews={undefined} busy={false} onClose={noop} />),
    ).toBe('');
    expect(renderToStaticMarkup(<AgentPreviews previews={[]} busy={false} onClose={noop} />)).toBe(
      '',
    );
  });

  test('one row per preview with the label, the monitor when known, and a Close button', () => {
    const html = renderToStaticMarkup(
      <AgentPreviews
        previews={[
          preview(),
          preview({ window_id: 'cd34', monitor: 'DP-1', caller_label: 'calm-owl' }),
        ]}
        busy={false}
        onClose={noop}
      />,
    );
    expect(html).toContain('Agent previews');
    expect(html).toContain('<span>bold-fox</span>');
    expect(html).toContain('<span>calm-owl · DP-1</span>');
    expect(html.match(/>Close<\/button>/g)?.length).toBe(2);
    expect(html).toContain('data-preview="ab12"');
    // No address and no URL.
    expect(html).not.toContain('0x');
    expect(html).not.toContain('http');
  });

  test('the text of a row', () => {
    expect(previewText(preview())).toBe('bold-fox');
    expect(previewText(preview({ monitor: 'HDMI-A-1' }))).toBe('bold-fox · HDMI-A-1');
  });

  test('a busy panel disables Close', () => {
    const html = renderToStaticMarkup(
      <AgentPreviews previews={[preview()]} busy={true} onClose={noop} />,
    );
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>Close<\/button>/);
  });
});

describe('the Desktop panel with previews', () => {
  const env = {
    settings: {} as never,
    accounts: {} as never,
    tailnet: {} as never,
    displays: {} as never,
    desktop: {} as never,
    capture: {} as never,
    deletion: {} as never,
  } as SettingsEnv;
  const render = (desktop: DesktopInfo) =>
    renderToStaticMarkup(
      <SettingsProvider env={env}>
        <DesktopPanel
          station={station('station-a', {
            info: { version: null, providers: [], desktop } as unknown as never,
          })}
        />
      </SettingsProvider>,
    );

  test('the section shows when the info has previews and is absent otherwise', () => {
    expect(render(info({ previews: [preview()] }))).toContain('Agent previews');
    expect(render(info())).not.toContain('Agent previews');
    expect(render(info({ previews: [] }))).not.toContain('Agent previews');
  });
});

describe('closing a preview', () => {
  test('Close sends the owner act body, then refreshes the info', async () => {
    const calls: string[] = [];
    const sent: DesktopActBody[] = [];
    const after = info({ previews: [] });
    const refreshed = info({ previews: [preview({ window_id: 'cd34' })] });
    const desktop: DesktopActions = {
      info: async (st) => {
        calls.push(`info ${st}`);
        return refreshed;
      },
      act: async (st, body) => {
        calls.push(`act ${st}`);
        sent.push(body);
        return { address: null, info: after };
      },
    };
    const seen: DesktopInfo[] = [];
    await closePreview(desktop, 'Box', 'ab12', (i) => seen.push(i));
    expect(sent).toEqual([{ action: 'preview_close', window_id: 'ab12' }]);
    expect(calls).toEqual(['act Box', 'info Box']);
    expect(seen).toEqual([after, refreshed]);
  });

  test('a failed close rejects with the error of the answer and does not refresh', async () => {
    let refreshed = false;
    const desktop: DesktopActions = {
      info: async () => {
        refreshed = true;
        return info();
      },
      act: async () => {
        throw new Error('No window opened by this grant matches that handle.');
      },
    };
    const seen: DesktopInfo[] = [];
    await expect(closePreview(desktop, 'Box', 'ab12', (i) => seen.push(i))).rejects.toThrow(
      'No window opened by this grant matches that handle.',
    );
    expect(refreshed).toBe(false);
    expect(seen).toEqual([]);
  });
});
