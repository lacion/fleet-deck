import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DesktopInfo } from '../gen';
import { SettingsProvider, type SettingsEnv } from '../settings/context';
import { station } from '../testing/fleet';
import {
  availability,
  CaptureBlock,
  CaptureImageView,
  FEATURE,
  IMAGE_FAILED_TEXT,
  IMAGE_LOAD_MS,
  REASONS,
  shotSrc,
} from './CapturePanel';
import { DesktopPanel } from './DesktopPanel';
import { SAVED_TEXT, UNKNOWN_TEXT, WAITING_TEXT } from './flow';

const info = (over: Partial<DesktopInfo> = {}): DesktopInfo => ({
  kind: 'omarchy',
  omarchy_version: '3.1',
  monitors: [
    {
      name: 'DP-1',
      description: 'Main',
      width: 3440,
      height: 1440,
      scale_pct: 100,
      refresh_hz: 100,
      focused: true,
      dpms_on: true,
    },
  ],
  locked: false,
  dnd: false,
  idle_inhibited: false,
  walls: [],
  allowed: { desktop: true, capture: true, capture_monitors: [], input: false },
  tools: {
    wtype: true,
    pointer: false,
    recorder: false,
    grim: true,
    chromium: true,
    notify_send: true,
  },
  features: [FEATURE],
  ...over,
});

const DECK = [FEATURE];
const noop = () => {};

describe('when the Screenshot button is enabled (decision 5)', () => {
  test('all conditions hold: enabled, no reason', () => {
    expect(availability(DECK, info(), 'DP-1', false)).toEqual({ enabled: true, reason: null });
  });

  test('the features of the deck are loading: disabled, no reason line', () => {
    expect(availability(undefined, info(), 'DP-1', false)).toEqual({
      enabled: false,
      reason: null,
    });
  });

  test('an old deck, or a deck without the feature', () => {
    expect(availability(null, info(), 'DP-1', false).reason).toBe(REASONS.deck);
    expect(availability(['other'], info(), 'DP-1', false).reason).toBe(REASONS.deck);
    expect(REASONS.deck).toBe('This deck does not support screenshots.');
  });

  test('a station without the feature, without grim, or with capture off', () => {
    expect(availability(DECK, info({ features: [] }), 'DP-1', false).reason).toBe(
      'This station does not support screenshots.',
    );
    const noGrim = info({ tools: { ...info().tools, grim: false } });
    expect(availability(DECK, noGrim, 'DP-1', false).reason).toBe(
      'The screenshot tool grim is not installed.',
    );
    const off = info({ allowed: { ...info().allowed, capture: false } });
    expect(availability(DECK, off, 'DP-1', false)).toEqual({
      enabled: false,
      reason: 'Desktop screenshots are off on this station. Turn them on in Settings.',
    });
  });

  test('a monitor that is not in the allow list', () => {
    const some = info({ allowed: { ...info().allowed, capture_monitors: ['HDMI-A-1'] } });
    expect(availability(DECK, some, 'DP-1', false).reason).toBe(
      'This monitor is not allowed for screenshots.',
    );
    expect(availability(DECK, some, 'HDMI-A-1', false).enabled).toBe(true);
  });

  test('a capture in progress', () => {
    expect(availability(DECK, info(), 'DP-1', true)).toEqual({
      enabled: false,
      reason: 'A screenshot is in progress.',
    });
  });

  test('the first reason wins', () => {
    const bad = info({ features: [], tools: { ...info().tools, grim: false } });
    expect(availability(null, bad, 'DP-1', true).reason).toBe(REASONS.deck);
    expect(availability(DECK, bad, 'DP-1', true).reason).toBe(REASONS.station);
  });
});

describe('the Desktop panel with the screenshot button', () => {
  const env = {
    settings: {} as never,
    accounts: {} as never,
    tailnet: {} as never,
    displays: {} as never,
    desktop: {} as never,
    capture: {} as never,
    deletion: {} as never,
  } as SettingsEnv;
  test('each monitor row has a Screenshot button; it is disabled until the features load', () => {
    const view = station('station-a', {
      info: { version: null, providers: [], desktop: info() } as unknown as never,
    });
    const html = renderToStaticMarkup(
      <SettingsProvider env={env}>
        <DesktopPanel station={view} />
      </SettingsProvider>,
    );
    expect(html).toContain('Screenshot');
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>Screenshot<\/button>/);
  });
});

describe('the capture block', () => {
  const block = (state: Parameters<typeof CaptureBlock>[0]['state']) =>
    renderToStaticMarkup(<CaptureBlock state={state} onCheck={noop} onDismiss={noop} />);
  const published = {
    artifact: { artifact_id: 'A 1', station_id: 'st/1', title: 'Desktop capture · Box/DP-1' },
    version: { version: 2, files: ['capture.png'] },
    created: true,
  } as never;

  test('waiting shows the status text', () => {
    const html = block({ kind: 'waiting', monitor: 'DP-1', captureId: 'C', after202: false });
    expect(html).toContain('role="status"');
    expect(html).toContain(WAITING_TEXT);
    expect(WAITING_TEXT).toBe('Waiting for the screenshot…');
  });

  test('unknown shows the text with Check result and Dismiss', () => {
    const html = block({ kind: 'unknown', monitor: 'DP-1', captureId: 'C', checking: false });
    expect(html).toContain(UNKNOWN_TEXT);
    expect(UNKNOWN_TEXT).toBe(
      'The screenshot result is unknown. Check the artifact list before taking another screenshot.',
    );
    expect(html).toContain('Check result');
    expect(html).toContain('Dismiss');
  });

  test('failed and deleted', () => {
    expect(block({ kind: 'failed', text: 'The screenshot tool failed.' })).toContain(
      'The screenshot tool failed.',
    );
    expect(block({ kind: 'deleted' })).toContain('The screenshot was deleted.');
  });

  test('committed: the link and the image; the saved note only after a 202', () => {
    const plain = block({ kind: 'committed', published, after202: false });
    expect(plain).toContain('href="#/artifact/st%2F1/A%201"');
    expect(plain).toContain('Open the screenshot');
    expect(plain).toContain('src="/api/artifacts/st%2F1/A%201/2/raw/capture.png"');
    expect(plain).not.toContain(SAVED_TEXT);
    const late = block({ kind: 'committed', published, after202: true });
    expect(late).toContain(
      'The screenshot was saved on the station. The artifact list is still updating.',
    );
    expect(late.indexOf(SAVED_TEXT)).toBeLessThan(late.indexOf('Open the screenshot'));
  });
});

describe('the screenshot image (§7.4)', () => {
  const published = (files: string[]) =>
    ({
      artifact: { artifact_id: 'a', station_id: 's', title: 't' },
      version: { version: 1, files },
      created: true,
    }) as never;

  test('the timer is 65 seconds', () => {
    expect(IMAGE_LOAD_MS).toBe(65_000);
  });

  test('"Try again" adds ?r=<n>; no file has no URL', () => {
    expect(shotSrc(published(['capture.png']), 0)).toBe('/api/artifacts/s/a/1/raw/capture.png');
    expect(shotSrc(published(['capture.png']), 2)).toBe('/api/artifacts/s/a/1/raw/capture.png?r=2');
    expect(shotSrc(published([]), 0)).toBeNull();
  });

  test('a failed load shows the text and Try again, and no image', () => {
    const html = renderToStaticMarkup(
      <CaptureImageView
        src="/x"
        alt="a"
        status="failed"
        onLoad={noop}
        onError={noop}
        onRetry={noop}
      />,
    );
    expect(html).toContain('role="alert"');
    expect(html).toContain('The screenshot could not be loaded. Try again.');
    expect(IMAGE_FAILED_TEXT).toBe('The screenshot could not be loaded. Try again.');
    expect(html).toContain('Try again</button>');
    expect(html).not.toContain('<img');
    const loading = renderToStaticMarkup(
      <CaptureImageView
        src="/x"
        alt="a"
        status="loading"
        onLoad={noop}
        onError={noop}
        onRetry={noop}
      />,
    );
    expect(loading).toContain('<img');
    expect(loading).not.toContain('role="alert"');
  });
});
