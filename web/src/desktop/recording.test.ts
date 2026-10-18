import { describe, expect, test } from 'bun:test';
import type { DesktopInfo } from '../gen';
import {
  ALLOWANCE_MS,
  availability,
  capability,
  DEFAULT_SECONDS,
  DISCLOSURE,
  DURATION_ERROR,
  DURATION_LABEL,
  PHASE_TEXT,
  parseSeconds,
  REASONS,
  RECORD_FEATURE,
  recordingKey,
  startOff,
  windowMs,
} from './recording';

const info = (over: Partial<DesktopInfo> = {}): DesktopInfo => ({
  kind: 'omarchy',
  omarchy_version: '3.1',
  monitors: [],
  locked: false,
  dnd: false,
  idle_inhibited: false,
  walls: [],
  allowed: { desktop: true, capture: true, capture_monitors: [], input: false },
  tools: {
    wtype: true,
    pointer: false,
    recorder: true,
    grim: true,
    chromium: true,
    notify_send: true,
  },
  features: ['desktop_capture.v1', RECORD_FEATURE, 'artifacts_video.v1'],
  ...over,
});

const DECK = [RECORD_FEATURE, 'artifacts_video.v1', 'desktop_capture.v1'];

describe('the recording duration', () => {
  test('a whole number from 1 to 120 is accepted; the default is 30', () => {
    expect(DEFAULT_SECONDS).toBe('30');
    expect(parseSeconds('30')).toBe(30);
    expect(parseSeconds('1')).toBe(1);
    expect(parseSeconds('120')).toBe(120);
  });

  test('anything else is refused', () => {
    for (const bad of ['', '0', '121', '-1', '1.5', '1e1', 'abc', ' 5', '5 ', '99999']) {
      expect(parseSeconds(bad)).toBeNull();
    }
  });

  test('the texts are exact', () => {
    expect(DURATION_LABEL).toBe('Duration (seconds)');
    expect(DURATION_ERROR).toBe('Recording duration must be a whole number from 1 to 120 seconds.');
    expect(PHASE_TEXT).toEqual({
      preparing: 'Preparing recording',
      recording: 'Recording',
      stopping: 'Stopping recording',
      finalizing: 'Saving recording',
    });
  });

  test('the window is the duration plus 45 seconds', () => {
    expect(ALLOWANCE_MS).toBe(45_000);
    expect(windowMs(30)).toBe(75_000);
    expect(windowMs(120)).toBe(165_000);
  });

  test('the disclosure is exact', () => {
    expect(DISCLOSURE).toBe(
      'Records the whole selected monitor, including other applications, notifications and private information visible there. No audio is recorded. The recording is stored as an ordinary artifact until deleted.',
    );
  });

  test('the key', () => {
    expect(recordingKey('st-1')).toBe('fd.recording.st-1');
  });
});

describe('when Record is enabled', () => {
  test('all conditions hold: enabled, no reason', () => {
    expect(availability(DECK, info(), 'DP-1')).toEqual({ enabled: true, reason: null });
  });

  test('the features of the deck are loading: off, no reason line', () => {
    expect(availability(undefined, info(), 'DP-1')).toEqual({ enabled: false, reason: null });
  });

  test('capability, tool and permission failures are separate', () => {
    expect(availability(null, info(), 'DP-1').reason).toBe(REASONS.deck);
    expect(availability(['desktop_capture.v1'], info(), 'DP-1').reason).toBe(REASONS.deck);
    expect(availability(DECK, info({ features: [] }), 'DP-1').reason).toBe(
      'This station does not support monitor recordings.',
    );
    const noTool = info({ tools: { ...info().tools, recorder: false } });
    expect(availability(DECK, noTool, 'DP-1').reason).toBe(
      'The recording tool wf-recorder is not installed.',
    );
    const off = info({ allowed: { ...info().allowed, capture: false } });
    expect(availability(DECK, off, 'DP-1')).toEqual({ enabled: false, reason: REASONS.off });
    const some = info({ allowed: { ...info().allowed, capture_monitors: ['HDMI-A-1'] } });
    expect(availability(DECK, some, 'DP-1').reason).toBe(REASONS.monitor);
    expect(availability(DECK, some, 'HDMI-A-1').enabled).toBe(true);
  });

  test('the capability line is the first reason', () => {
    expect(capability(DECK, info())).toBeNull();
    expect(capability(null, info())).toBe(REASONS.deck);
    const bad = info({ features: [], tools: { ...info().tools, recorder: false } });
    expect(availability(DECK, bad, 'DP-1').reason).toBe(REASONS.station);
  });
});

describe('the shared busy state', () => {
  test('a start is off while busy, while the slot is active, or when not enabled', () => {
    expect(startOff({ busy: false, slotActive: false, enabled: true })).toBe(false);
    expect(startOff({ busy: true, slotActive: false, enabled: true })).toBe(true);
    expect(startOff({ busy: false, slotActive: true, enabled: true })).toBe(true);
    expect(startOff({ busy: false, slotActive: false, enabled: false })).toBe(true);
  });
});
