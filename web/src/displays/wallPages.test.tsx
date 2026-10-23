// B17 §3.2.8: the wall pages of Settings › Displays and "Show a page…". A sibling of
// `displays.test.tsx` (that file holds the tests of the displays; this one stays under 800 lines).
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DisplayView, WallPage } from '../gen';
import type { SettingsActions } from '../settings/actions';
import type { SettingsEnv } from '../settings/context';
import { DeckError, type DeckIo } from '../settings/http';
import { station } from '../testing/fleet';
import { displayActions, type DisplayActions } from './actions';
import { DisplaysView } from './DisplaysTab';
import { ELEVATION_LABELS } from './elevate';
import { NO_PAGES_TEXT, pageDirective, SHOWN_TEXT, sendPage, ShowPageForm } from './ShowPage';
import {
  addPage,
  DELETE_CONFIRM,
  DeleteConfirm,
  EMPTY_TEXT,
  FRAMING_NOTE,
  HTTP_NOTE,
  INTRO_TEXT,
  NAME_PROBLEM,
  PageList,
  SECRET_WARNING,
} from './WallPages';

type Call = { method: 'GET' | 'POST'; path: string; body?: unknown };

function fakeIo(answers: Record<string, unknown> = {}) {
  const calls: Call[] = [];
  const io: DeckIo = {
    get: async (path) => {
      calls.push({ method: 'GET', path });
      return answers[path] ?? null;
    },
    post: async (path, body) => {
      calls.push({ method: 'POST', path, body });
      return answers[path] ?? null;
    },
  };
  return { io, calls };
}

const display = (): DisplayView => ({
  display_id: 'D'.repeat(26),
  name: 'Wall',
  ceiling: ['station-a'],
  show_diffs: false,
  wake_on_needs_you: true,
  quiet_hours: null,
  binding: null,
  created_at: 1_000,
  last_seen: null,
});

const stationsList = [station('station-a')];

describe('the wall pages', () => {
  const entry = (id: string, label = 'Grafana'): WallPage => ({
    page_id: id,
    label,
    url: 'https://grafana.example:3000/d/abc?token=x',
    origin: 'https://grafana.example:3000',
    fit: 'long',
    created_at: 1,
  });
  const form = { label: ' Grafana ', url: ' https://grafana.example/ ', fit: 'screen' } as const;
  const settings = (log: string[]): SettingsActions =>
    ({
      elevation: () => null,
      forgetElevation: () => log.push('forget'),
    }) as unknown as SettingsActions;
  const invalid = (text: string) => new DeckError(400, text, 'invalid_page', {});

  test('the calls are validated and the add body carries the fields and the elevation', async () => {
    const { io } = fakeIo({
      '/api/wall-pages': { pages: [entry('A')] },
      '/api/wall-pages/A%2F1/delete': { displays_reset: 2 },
    });
    const actions = displayActions(io);
    expect((await actions.wallPages()).pages).toHaveLength(1);
    expect(await actions.deleteWallPage('A/1')).toBe(2);
    // The add answers the entry on the same path as the list.
    const add = fakeIo({ '/api/wall-pages': entry('A') });
    await displayActions(add.io).addWallPage(
      { label: 'G', url: 'https://g.example/', fit: 'long' },
      'el',
    );
    expect(add.calls[0]).toEqual({
      method: 'POST',
      path: '/api/wall-pages',
      body: { label: 'G', url: 'https://g.example/', fit: 'long', elevation: 'el' },
    });
    await expect(displayActions(fakeIo({ '/api/wall-pages': {} }).io).wallPages()).rejects.toThrow(
      'The deck sent an invalid wall page list.',
    );
  });

  test('add_page_asks_for_elevation', async () => {
    const sent: (string | null)[] = [];
    const log: string[] = [];
    const asked: (readonly string[] | undefined)[] = [];
    const actions = {
      addWallPage: async (page: unknown, elevation: string | null) => {
        sent.push(elevation);
        expect(page).toEqual({ label: 'Grafana', url: 'https://grafana.example/', fit: 'screen' });
        if (elevation === null) {
          throw new DeckError(400, 'a recent sign-in is needed', 'elevation_required', {
            keys: ['wall_pages'],
          });
        }
        return entry('A');
      },
    } as unknown as DisplayActions;
    const ask = async (labels?: readonly string[]) => {
      asked.push(labels);
      return 'tok';
    };
    expect(await addPage(actions, settings(log), ask, form)).toEqual({ kind: 'added' });
    expect(sent).toEqual([null, 'tok']);
    expect(asked).toEqual([ELEVATION_LABELS]);
    expect(log).toEqual(['forget']);
    // The owner cancels the dialog: nothing is added.
    expect(await addPage(actions, settings([]), async () => null, form)).toEqual({
      kind: 'cancelled',
    });
  });

  test('the_deck_error_text_shows_under_the_field', async () => {
    const fail = (e: unknown) =>
      ({ addWallPage: async () => Promise.reject(e) }) as unknown as DisplayActions;
    const run = (e: unknown) => addPage(fail(e), settings([]), async () => 'tok', form);
    expect(await run(invalid(NAME_PROBLEM))).toEqual({
      kind: 'error',
      field: 'name',
      text: 'Give a name of 1 to 60 characters.',
    });
    expect(await run(invalid('Give an http or https address.'))).toEqual({
      kind: 'error',
      field: 'address',
      text: 'Give an http or https address.',
    });
    expect(
      await run(
        invalid('This is the address of the board. A wall cannot show the board in a frame.'),
      ),
    ).toMatchObject({ field: 'address' });
    // The limit and the transport errors belong to no field.
    expect(
      await run(new DeckError(409, 'A deck holds at most 50 wall pages.', 'too_many_pages', {})),
    ).toEqual({ kind: 'error', field: null, text: 'A deck holds at most 50 wall pages.' });
  });

  test('delete_confirms_with_the_exact_text', () => {
    expect(DELETE_CONFIRM).toBe('Delete this page? Walls that show it go back to Mission view.');
    const html = renderToStaticMarkup(
      <DeleteConfirm busy={false} onDelete={() => {}} onKeep={() => {}} />,
    );
    expect(html).toContain('Delete this page? Walls that show it go back to Mission view.');
    expect(html).toContain('>Delete</button>');
    expect(html).toContain('>Keep</button>');
    expect(html).toContain('class="fd-danger"');
    const list = renderToStaticMarkup(
      <PageList
        state={{ kind: 'ready', pages: [entry('A'), { ...entry('B', 'Docs'), fit: 'screen' }] }}
        onDelete={async () => null}
        onRetry={() => {}}
      />,
    );
    expect(list).toContain('Delete…');
    expect(list).toContain('class="fd-note fd-wp-host">grafana.example<');
    expect(list).toContain('Long page');
    expect(list).toContain('One screen');
    expect(list).not.toContain('token=x');
    expect(list).not.toContain(DELETE_CONFIRM);
  });

  test('show_a_page_sends_a_page_directive', async () => {
    expect(pageDirective('PAGE', true)).toEqual({
      mode: 'page',
      page: { page_id: 'PAGE' },
      pin: true,
      filter: null,
    });
    const calls: [string, unknown][] = [];
    const actions = {
      directive: async (id: string, body: unknown) => {
        calls.push([id, body]);
        return { generation: 3, directive: body };
      },
    } as unknown as DisplayActions;
    await sendPage(actions, 'D1', 'PAGE', false);
    expect(calls).toEqual([
      ['D1', { mode: 'page', page: { page_id: 'PAGE' }, pin: false, filter: null }],
    ]);
    const base = {
      chosen: 'A',
      pin: true,
      busy: false,
      error: null,
      onChoose: () => {},
      onPin: () => {},
      onSubmit: () => {},
      onCancel: () => {},
    };
    const html = renderToStaticMarkup(
      <ShowPageForm {...base} loaded={{ kind: 'ready', pages: [entry('A')] }} shown={false} />,
    );
    expect(html).toContain('Grafana');
    expect(html).toContain('Keep it on the screen (no auto-focus)');
    expect(html).toContain('checked=""');
    expect(html).toContain('class="fd-primary"');
    expect(html).toContain('Show the page');
    const done = renderToStaticMarkup(
      <ShowPageForm {...base} loaded={{ kind: 'ready', pages: [entry('A')] }} shown />,
    );
    expect(done).toContain(SHOWN_TEXT);
    expect(SHOWN_TEXT).toBe('The wall shows the page.');
  });

  test('no_pages_shows_the_empty_text', () => {
    expect(NO_PAGES_TEXT).toBe('No wall pages yet. Add one under Wall pages.');
    const dialog = renderToStaticMarkup(
      <ShowPageForm
        loaded={{ kind: 'ready', pages: [] }}
        chosen=""
        pin
        busy={false}
        error={null}
        shown={false}
        onChoose={() => {}}
        onPin={() => {}}
        onSubmit={() => {}}
        onCancel={() => {}}
      />,
    );
    expect(dialog).toContain(NO_PAGES_TEXT);
    expect(dialog).toMatch(
      /<button[^>]*class="fd-primary"[^>]*disabled=""|<button[^>]*disabled=""[^>]*class="fd-primary"/,
    );
    expect(EMPTY_TEXT).toBe('No wall pages yet.');
    const list = renderToStaticMarkup(
      <PageList
        state={{ kind: 'ready', pages: [] }}
        onDelete={async () => null}
        onRetry={() => {}}
      />,
    );
    expect(list).toContain('No wall pages yet.');
  });

  test('the section shows only on a deck with wall pages, with its texts and its form', () => {
    const env = {
      settings: {} as SettingsActions,
      displays: {} as DisplayActions,
    } as SettingsEnv;
    const page = (wallPages: boolean) =>
      renderToStaticMarkup(
        <DisplaysView
          state={{ kind: 'ready', list: { displays: [display()], pending: [] } }}
          refresh={() => {}}
          env={env}
          stations={stationsList}
          ask={async () => null}
          now={61_000}
          wallPages={wallPages}
        />,
      );
    expect(page(false)).not.toContain('Wall pages');
    const html = page(true);
    for (const text of [INTRO_TEXT, FRAMING_NOTE, HTTP_NOTE, SECRET_WARNING]) {
      expect(html).toContain(text.replace(/'/g, '&#x27;'));
    }
    expect(html).toContain('Add the page');
    expect(html).toContain('Fits one screen');
    expect(html).toContain('Long page (scrolls)');
    expect(html).toContain('>Name<');
    expect(html).toContain('>Address<');
  });
});
