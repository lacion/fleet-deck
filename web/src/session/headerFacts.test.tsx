// The live UI audit of the session (round 1): the phone Send and Resume (one primary per screen),
// the folder chip room, and the Changes tab count of a session view.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DeckSession } from '../gen';
import type { Timeline } from '../runtime/state';
import { claudeCaps } from '../testing/capabilities';
import { card, station } from '../testing/fleet';
import { sessionActions } from './commands';
import { Composer } from './Composer';
import { sessionFlags } from './controller';
import { roomChars, shortDir } from './PathFit';
import { SessionHeader } from './SessionHeader';
import { SessionView } from './SessionView';
import { setDiffTotal } from './workspace';

const key = { station_id: 'station-a', session_id: 's1' };
const drive = sessionActions(async () => ({}), key, ['read', 'drive', 'approve']);
const noop = () => {};

function session(
  extra: Partial<DeckSession['summary']> = {},
  status: DeckSession['status'] = 'working',
): DeckSession {
  return card('s1', status, 'station-a', {
    title: 'Fix login',
    capabilities: claudeCaps(),
    current_run: 'run-1',
    ...extra,
  });
}

const lost = () => session({ state: 'lost', current_run: null }, 'lost');

const timeline: Timeline = {
  incarnation: 'inc',
  cursor: 0,
  events: [],
  freshness: 'live',
  error: null,
  ready: true,
};

describe('the composer send button (§4.8)', () => {
  const composer = (extra: Partial<Parameters<typeof Composer>[0]> = {}) =>
    renderToStaticMarkup(
      <Composer
        enabled
        working={false}
        steer={false}
        nativeQueue={false}
        onSend={noop}
        {...extra}
      />,
    );

  test('the phone Send is a round icon button named Send; the desktop Send keeps its text', () => {
    const phone = composer({ phone: true });
    expect(phone).toMatch(
      /<button type="submit" class="fd-iconbtn fd-primary fd-composer-go" aria-label="Send" title="Send" disabled=""><svg/,
    );
    expect(phone).not.toContain('> Send');
    expect(composer()).toMatch(/<button type="submit" class="fd-primary" disabled="">.*> Send/);
    // Steer now and Queue follow-up keep their words on the phone (they share the row).
    const working = composer({ phone: true, working: true, steer: true });
    expect(working).toContain('Steer now');
    expect(working).toContain('Queue follow-up');
    expect(working).not.toContain('fd-composer-go');
  });
});

describe('one Resume primary on the phone (§2.2)', () => {
  const header = (composerResume: boolean, layout: 'phone' | 'desktop' = 'phone') =>
    renderToStaticMarkup(
      <SessionHeader
        session={lost()}
        stations={[station()]}
        controller={{
          flags: sessionFlags(lost(), 'live', drive),
          busy: null,
          outcome: null,
          act: noop,
          dismiss: noop,
        }}
        actions={drive}
        terminal={null}
        layout={layout}
        composerResume={composerResume}
      />,
    );

  test('the phone header drops its Resume while the composer shows the Resume bar', () => {
    expect(header(true)).not.toContain('aria-label="Resume"');
    expect(header(false)).toContain('aria-label="Resume"');
    // The desktop header keeps its primary Resume.
    expect(header(true, 'desktop')).toMatch(/class="fd-primary"[^>]*>.*Resume<\/button>/);
  });

  test('a lost session says that its process stopped; the phone bar has the primary', () => {
    const html = renderToStaticMarkup(
      <SessionView
        session={lost()}
        stations={[station()]}
        freshness="live"
        timeline={timeline}
        actions={drive}
        layout="phone"
        tab="chat"
      />,
    );
    expect(html).toContain('The session was lost: its process stopped. Resume it to send prompts.');
    expect(html).not.toContain('The session ended.');
    expect(html).toMatch(/<div class="fd-composer-resume">.*class="fd-primary"/);
    expect(html).not.toContain('aria-label="Resume"');
  });
});

describe('the folder chip room', () => {
  test('roomChars: the characters that fit, never less than none', () => {
    expect(roomChars(100, 7.2)).toBe(13);
    expect(roomChars(-40, 7.2)).toBe(0);
    expect(roomChars(100, 0)).toBeNull();
  });

  test('shortDir: the folders of a file drop whole leading names, never a part of one', () => {
    const dir = 'crates/fleetdeck-station/src/shim/';
    expect(shortDir(dir, 40)).toBe(dir);
    expect(shortDir(dir, 14)).toBe('…/src/shim/');
    expect(shortDir(dir, 8)).toBe('…/shim/');
    expect(shortDir(dir, 4)).toBe('…/');
    expect(shortDir(dir, -3)).toBe('…/');
  });
});

describe('the Changes tab count (§4.5)', () => {
  const view = (s: DeckSession) =>
    renderToStaticMarkup(
      <SessionView
        session={s}
        stations={[station()]}
        freshness="live"
        timeline={timeline}
        actions={drive}
        layout="tablet"
        tab="chat"
      />,
    );

  test('the label counts the changed files of the loaded session diff', () => {
    const s = card('c1', 'idle', 'station-a', {
      title: 'Counted',
      capabilities: claudeCaps(),
      base: { git_ref: 'main', oid: '0123456789abcdef0123456789abcdef01234567', inferred: false },
    });
    // Not loaded yet: no count (the turns of a shared folder would say 0).
    expect(view(s)).toMatch(/role="tab"[^>]*>Changes<\/button>/);
    setDiffTotal('station-a/c1', { files_changed: 3, insertions: 1286, deletions: 400 }, ['a']);
    expect(view(s)).toMatch(/role="tab"[^>]*>Changes <span class="fd-count">3<\/span><\/button>/);
    setDiffTotal('station-a/c1', { files_changed: 0, insertions: 0, deletions: 0 }, []);
    expect(view(s)).toMatch(/role="tab"[^>]*>Changes<\/button>/);
  });
});
