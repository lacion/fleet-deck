import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DesktopInfo } from '../gen';
import { type SettingsEnv, SettingsProvider } from '../settings/context';
import { station } from '../testing/fleet';
import { DesktopPanel } from './DesktopPanel';
import { RecordBlock, RecordPanel } from './RecordPanel';
import type { RecState } from './recordFlow';
import { DISCLOSURE, DURATION_LABEL, UNKNOWN_TEXT } from './recording';

const noop = () => {};

const info = (): DesktopInfo => ({
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
    {
      name: 'HDMI-A-1',
      description: 'Side',
      width: 1920,
      height: 1080,
      scale_pct: 100,
      refresh_hz: 60,
      focused: false,
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
    recorder: true,
    grim: true,
    chromium: true,
    notify_send: true,
  },
  features: ['desktop_capture.v1', 'desktop_recording.v1', 'artifacts_video.v1'],
});

const block = (state: RecState) =>
  renderToStaticMarkup(<RecordBlock state={state} onStop={noop} onCheck={noop} />);

const running = (over: Partial<Extract<RecState, { kind: 'running' }>> = {}): RecState => ({
  kind: 'running',
  monitor: 'DP-1',
  captureId: 'R1',
  phase: 'recording',
  posting: false,
  after202: false,
  error: null,
  ...over,
});

describe('the Record control of a monitor', () => {
  const html = (off: boolean) =>
    renderToStaticMarkup(<RecordPanel monitor="DP-1" off={off} onStart={noop} />);

  test('the duration input defaults to 30 and is a labeled whole number from 1 to 120', () => {
    const out = html(false);
    expect(out).toContain(`>${DURATION_LABEL}</label>`);
    expect(DURATION_LABEL).toBe('Duration (seconds)');
    expect(out).toContain('value="30"');
    expect(out).toContain('min="1"');
    expect(out).toContain('max="120"');
    expect(out).toContain('type="number"');
    expect(out).toContain('required=""');
  });

  test('the disclosure is always visible under the control, exactly', () => {
    for (const off of [false, true]) {
      const out = html(off);
      expect(out).toContain(DISCLOSURE);
      expect(out.indexOf('Record</button>')).toBeLessThan(out.indexOf(DISCLOSURE));
    }
    expect(DISCLOSURE).toBe(
      'Records the whole selected monitor, including other applications, notifications and private information visible there. No audio is recorded. The recording is stored as an ordinary artifact until deleted.',
    );
  });

  test('Record is enabled, or disabled by the shared busy state', () => {
    expect(html(false)).toMatch(/<button[^>]*>Record<\/button>/);
    expect(html(false)).not.toMatch(/<button[^>]*disabled=""[^>]*>Record<\/button>/);
    expect(html(true)).toMatch(/<button[^>]*disabled=""[^>]*>Record<\/button>/);
  });
});

describe('the recording block', () => {
  test('the four phases, from the station status', () => {
    const text = {
      preparing: 'Preparing recording',
      recording: 'Recording',
      stopping: 'Stopping recording',
      finalizing: 'Saving recording',
    } as const;
    for (const [phase, want] of Object.entries(text)) {
      const out = block(running({ phase: phase as keyof typeof text }));
      expect(out).toContain('role="status"');
      expect(out).toContain(`>${want}</p>`);
    }
    expect(block(running({ phase: null }))).toContain('>Preparing recording</p>');
  });

  test('Stop recording is on whenever an ID is known, never disabled', () => {
    const states: RecState[] = [
      running(),
      running({ phase: null, posting: true }),
      { kind: 'expired', monitor: 'DP-1', captureId: 'R1', checking: false, error: null },
      { kind: 'unreachable', captureId: 'R1', error: null },
    ];
    for (const state of states) {
      const out = block(state);
      expect(out).toMatch(/<button[^>]*>Stop recording<\/button>/);
      expect(out).not.toMatch(/<button[^>]*disabled=""[^>]*>Stop recording<\/button>/);
    }
  });

  test('no ID, no Stop recording', () => {
    expect(block({ kind: 'unreachable', captureId: null, error: null })).not.toContain(
      'Stop recording',
    );
    expect(block({ kind: 'idle' })).toBe('');
    expect(block({ kind: 'discovering' })).toBe('');
  });

  test('the station is not connected', () => {
    expect(block({ kind: 'unreachable', captureId: 'R1', error: null })).toContain(
      'The station is not connected.',
    );
  });

  test('the window end shows the unknown text and Check result', () => {
    const out = block({
      kind: 'expired',
      monitor: 'DP-1',
      captureId: 'R1',
      checking: false,
      error: null,
    });
    expect(out).toContain(UNKNOWN_TEXT);
    expect(UNKNOWN_TEXT).toBe(
      'The recording result is unknown. Check the artifact list before starting another recording.',
    );
    expect(out).toContain('Check result');
    expect(out).not.toMatch(/disabled=""[^>]*>Check result/);
    const checking = block({
      kind: 'expired',
      monitor: 'DP-1',
      captureId: 'R1',
      checking: true,
      error: null,
    });
    expect(checking).toMatch(/<button[^>]*disabled=""[^>]*>Check result<\/button>/);
  });

  test('a stop error is shown with the deck text', () => {
    expect(block(running({ error: 'This capture is not a recording.' }))).toContain(
      'This capture is not a recording.',
    );
  });

  test('committed: the link, and the saved note only after a 202; failed and deleted', () => {
    const published = {
      artifact: { artifact_id: 'A 1', station_id: 'st/1', title: 't' },
      version: { version: 1, files: ['recording.mp4'] },
      created: true,
    } as never;
    const plain = block({ kind: 'committed', published, after202: false });
    expect(plain).toContain('href="#/artifact/st%2F1/A%201"');
    expect(plain).toContain('Open the recording');
    expect(plain).not.toContain('still updating');
    expect(block({ kind: 'committed', published, after202: true })).toContain(
      'The recording was saved on the station. The artifact list is still updating.',
    );
    expect(block({ kind: 'failed', text: 'The recording tool failed.' })).toContain(
      'The recording tool failed.',
    );
    expect(block({ kind: 'deleted' })).toContain('The recording was deleted.');
  });
});

describe('the Desktop panel with the recording controls', () => {
  const env = {
    settings: {} as never,
    accounts: {} as never,
    tailnet: {} as never,
    displays: {} as never,
    desktop: {} as never,
    capture: {} as never,
    deletion: {} as never,
  } as SettingsEnv;

  test('each monitor row has Duration, Record and the disclosure; both starts are off while the deck loads', () => {
    const view = station('station-a', {
      info: { version: null, providers: [], desktop: info() } as unknown as never,
    });
    const html = renderToStaticMarkup(
      <SettingsProvider env={env}>
        <DesktopPanel station={view} />
      </SettingsProvider>,
    );
    expect(html.split('>Record</button>').length - 1).toBe(2);
    expect(html.split(DISCLOSURE).length - 1).toBe(2);
    expect(html.split(`>${DURATION_LABEL}</label>`).length - 1).toBe(2);
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>Record<\/button>/);
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>Screenshot<\/button>/);
  });
});
