// FG-12 (design §15.6, §15.7): the Artifacts link and the timeline lines of the artifact events.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { Event } from '../gen';
import { envelopes } from '../testing/timeline';
import { ArtifactsLink, artifactsRevoked, machineOfUrl } from './ArtifactsLink';
import { buildTimeline } from './timeline';
import type { FileActions } from './workspace';

const files: FileActions = {
  list: async () => ({ path: '', entries: [], truncated: false }) as never,
  read: async () => ({}) as never,
  search: async () => ({}) as never,
  diff: async () => ({}) as never,
  sessionDiff: async () => ({}) as never,
};

const ready = (extra: Record<string, string> = {}): Event =>
  ({ kind: 'artifacts.ready', ...extra }) as Event;
const revoked = (): Event =>
  ({
    kind: 'artifacts.revoked',
    code: 'share_unverified',
    text: 'The artifact share is not verified now. Fleet Deck stops showing the artifact folder.',
  }) as Event;

const link = (
  dir: Parameters<typeof ArtifactsLink>[0]['dir'],
  events: Event[] = [],
  f: FileActions | null = files,
) => renderToStaticMarkup(<ArtifactsLink dir={dir} events={envelopes('s1', events)} files={f} />);

describe('the Artifacts link', () => {
  test('a local folder shows the Artifacts button', () => {
    const html = link({ path: '/mnt/out/atlas/ada/' }, [ready({ path: '/mnt/out/atlas/ada/' })]);
    expect(html).toContain('>Artifacts</button>');
    expect(html).not.toContain('Copy address');
  });

  test('an address only shows the text and Copy address', () => {
    const url = 'http://100.100.100.100:8080/tail.ts.net/nas/out/atlas/ada/';
    const html = link({ url }, [ready({ url })]);
    expect(html).toContain('The artifacts are on nas. Open this address on a tailnet device:');
    expect(html).toContain(url);
    expect(html).toContain('Copy address');
    expect(html).not.toContain('>Artifacts</button>');
  });

  test('while the newest artifact event is revoked it shows the note and opens nothing', () => {
    const html = link(undefined, [ready({ path: '/mnt/out/a/' }), revoked()]);
    expect(html).toContain('The artifact share is not verified now.');
    expect(html).not.toContain('Artifacts</button>');
    // A later artifacts.ready shows the link again.
    const again = link({ path: '/mnt/out/a/' }, [revoked(), ready({ path: '/mnt/out/a/' })]);
    expect(again).toContain('>Artifacts</button>');
  });

  test('no folder, or no reader for a folder, shows nothing', () => {
    expect(link(undefined)).toBe('');
    expect(link({ path: '/mnt/out/a/' }, [], null)).toBe('');
    expect(link({})).toBe('');
  });

  test('the revoked state is the newest of the two events', () => {
    const at = (events: Event[]) => artifactsRevoked(envelopes('s1', events));
    expect(at([])).toBe(false);
    expect(at([revoked()])).toBe(true);
    expect(at([revoked(), ready()])).toBe(false);
    expect(at([ready(), revoked(), { kind: 'session.ended' } as Event])).toBe(true);
  });

  test('the machine is the second segment of the address', () => {
    expect(machineOfUrl('http://100.100.100.100:8080/t.ts.net/my%20nas/out/p/c/')).toBe('my nas');
    expect(machineOfUrl('http://100.100.100.100:8080/only')).toBeNull();
    expect(machineOfUrl('not a url')).toBeNull();
  });
});

describe('the chat lines of the artifact events', () => {
  const notices = (events: Event[]) =>
    buildTimeline(envelopes('s1', events)).items.flatMap((i) =>
      i.type === 'notice' ? [{ text: i.text, tone: i.tone }] : [],
    );

  test('artifacts.unavailable and artifacts.revoked show the text of the station', () => {
    const text =
      'The artifact share exposes protected folders; the session started without FLEETDECK_ARTIFACTS.';
    expect(
      notices([{ kind: 'artifacts.unavailable', code: 'share_exposed', text } as Event]),
    ).toEqual([{ text, tone: 'info' }]);
    expect(notices([revoked()])).toEqual([
      {
        text: 'The artifact share is not verified now. Fleet Deck stops showing the artifact folder.',
        tone: 'error',
      },
    ]);
    expect(notices([ready({ path: '/x/' })])).toEqual([]);
  });
});
