// R13 P5b (§12): the board video viewer, the recording provenance line and the retry.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { CaptureProvenance } from '../gen';
import { ArtifactViewerView } from './ArtifactViewer';
import { detail, NOW } from './fixtures';
import { FEATURES_HEADER, provenanceLine, reloadSource, VIDEO_FEATURES } from './video';
import { UNPLAYABLE_TEXT, VideoFrame } from './VideoView';
import type { ViewerData } from './viewer';

const RAW = '/api/artifacts/station-a/v1/2/raw/rec.mp4';

const recording = (
  over: Partial<NonNullable<CaptureProvenance['recording']>> = {},
): CaptureProvenance => ({
  capture_id: 'c1',
  station_id: 'station-a',
  monitor: 'DP-1',
  kind: 'recording',
  caller: 'owner',
  caller_station: null,
  at: NOW,
  recording: {
    requested_seconds: 30,
    duration_ms: 30_000,
    width: 1920,
    height: 1080,
    stop_reason: 'duration',
    ...over,
  },
});

const videoDetail = (capture?: CaptureProvenance) => {
  const d = detail('v1', { kind: 'video', title: 'Demo' }, [], ['rec.mp4']);
  if (capture === undefined) return d;
  return {
    ...d,
    versions: d.versions.map((v) => (v.version === 2 ? { ...v, capture } : v)),
  };
};

// The view link is still loading: a video does not use it.
const ready = (d = videoDetail()): ViewerData => ({
  kind: 'ok',
  value: { detail: d, version: 2, link: { kind: 'loading' }, text: null },
});

const render = (data: ViewerData) =>
  renderToStaticMarkup(
    <ArtifactViewerView
      data={data}
      variant="panel"
      canShare={false}
      onVersion={() => {}}
      onBack={() => {}}
      onShare={() => {}}
    />,
  );

const frame = (failed: boolean, provenance: string | null = null) =>
  renderToStaticMarkup(
    <VideoFrame
      url={RAW}
      title="Demo"
      provenance={provenance}
      failed={failed}
      downloadName="rec.mp4"
    />,
  );

describe('the declaration', () => {
  test('the feature and the header are the ones of the contract', () => {
    expect(VIDEO_FEATURES).toBe('artifacts_video.v1');
    expect(FEATURES_HEADER).toBe('X-FleetDeck-Features');
  });
});

describe('the video element', () => {
  test('native controls, metadata preload, no autoplay, loop or poster', () => {
    const html = frame(false);
    expect(html).toContain('<video');
    expect(html).toContain('controls=""');
    expect(html).toContain('preload="metadata"');
    expect(html).not.toMatch(/autoplay|loop|poster/i);
    expect(html).not.toContain('<iframe');
  });

  test('the source is the version-pinned raw path, with no view link and no blob', () => {
    const html = frame(false);
    expect(html).toContain(`src="${RAW}"`);
    expect(html).not.toContain('blob:');
    expect(html).not.toContain('data:');
  });

  test('the viewer renders the player for kind video while the view link is still loading', () => {
    const html = render(ready());
    expect(html).toContain('<video');
    expect(html).toContain(`src="${RAW}"`);
    expect(html).not.toContain('Opening the artifact…');
    expect(html).not.toContain('<iframe');
  });

  test('a version with no file list shows the note and no player', () => {
    const d = videoDetail();
    const html = render(ready({ ...d, versions: d.versions.map((v) => ({ ...v, files: [] })) }));
    expect(html).not.toContain('<video');
    expect(html).toContain('This version has no files yet.');
  });
});

describe('a playback error', () => {
  test('without an error there is no error text and no Retry', () => {
    const html = frame(false);
    expect(html).not.toContain(UNPLAYABLE_TEXT);
    expect(html).not.toContain('Retry');
  });

  test('the text, the Download action and Retry appear', () => {
    const html = frame(true);
    expect(UNPLAYABLE_TEXT).toBe('This browser cannot play this video.');
    expect(html).toContain('role="alert"');
    expect(html).toContain(UNPLAYABLE_TEXT);
    expect(html).toContain(`href="${RAW}"`);
    expect(html).toContain('download="rec.mp4"');
    expect(html).toContain('Download');
    expect(html).toContain('>Retry</button>');
  });

  test('Retry sets the src again and reloads; it starts no other call', () => {
    const calls: string[] = [];
    reloadSource(
      {
        removeAttribute: (n: string) => calls.push(`remove ${n}`),
        setAttribute: (n: string, v: string) => calls.push(`set ${n}=${v}`),
        load: () => calls.push('load'),
      },
      RAW,
    );
    expect(calls).toEqual(['remove src', `set src=${RAW}`, 'load']);
  });
});

describe('the provenance line', () => {
  const at = new Date(NOW).toLocaleString();

  test('a recording: station, monitor and local time', () => {
    expect(provenanceLine('Alpha', recording())).toBe(`Desktop recording · Alpha/DP-1 · ${at}`);
  });

  test('no note when the lengths differ by one second or less', () => {
    expect(provenanceLine('Alpha', recording({ duration_ms: 29_000 }))).not.toContain('requested');
    expect(provenanceLine('Alpha', recording({ duration_ms: 31_000 }))).not.toContain('requested');
  });

  test('a note when the lengths differ by more than one second', () => {
    expect(provenanceLine('Alpha', recording({ duration_ms: 12_340 }))).toBe(
      `Desktop recording · Alpha/DP-1 · ${at} · requested 30 s, saved 12.3 s`,
    );
    expect(provenanceLine('Alpha', recording({ duration_ms: 12_000 }))).toContain('saved 12 s');
    expect(provenanceLine('Alpha', recording({ duration_ms: 31_001 }))).toContain('saved 31 s');
  });

  test('a screenshot or no capture has no line', () => {
    expect(provenanceLine('Alpha', undefined)).toBeNull();
    expect(provenanceLine('Alpha', { ...recording(), kind: 'screenshot' })).toBeNull();
  });

  test('the viewer shows the line above the player', () => {
    const html = render(ready(videoDetail(recording({ duration_ms: 12_000 }))));
    expect(html).toContain('Desktop recording · Alpha/DP-1');
    expect(html).toContain('requested 30 s, saved 12 s');
    expect(html.indexOf('Desktop recording')).toBeLessThan(html.indexOf('<video'));
  });

  test('a video with no recording provenance shows no line', () => {
    expect(render(ready())).not.toContain('Desktop recording');
  });
});
