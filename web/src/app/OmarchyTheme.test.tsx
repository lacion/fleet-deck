// R13 P4 task F2: the Omarchy choice in Prefs: the station list, the stored choice and the select.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { PaletteColors } from '../gen';
import { ApiError } from '../runtime/api';
import { station } from '../testing/fleet';
import {
  OMARCHY_KEY,
  OMARCHY_LABEL,
  OmarchyThemeSelect,
  probeStations,
  readOmarchyChoice,
  setOmarchyChoice,
  writeOmarchyChoice,
} from './OmarchyTheme';
import { PrefsRow } from './Prefs';

const colors: PaletteColors = {
  background: '#1a1b26',
  foreground: '#c0caf5',
  accent: '#7aa2f7',
  muted: '#565f89',
  dark_background: '#16161e',
  dark_foreground: '#a9b1d6',
  bright_foreground: '#ffffff',
  selection: '#283457',
  red: '#f7768e',
  yellow: '#e0af68',
};
const body = { mode: 'dark', colors, updated_at: 1 };

describe('the station list', () => {
  const answers = (table: Record<string, 'ok' | 404 | 'error'>) => async (id: string) => {
    const v = table[id];
    if (v === 'ok') return body;
    if (v === 404) throw new ApiError(404, 'no palette', 'no_palette');
    throw new ApiError(500, 'boom');
  };

  test('a station with a 200 is listed and a 404 is dropped', async () => {
    const list = await probeStations(answers({ a: 'ok', b: 404, c: 'ok' }), ['a', 'b', 'c'], []);
    expect(list).toEqual(['a', 'c']);
  });

  test('a 404 drops a station that was listed before', async () => {
    expect(await probeStations(answers({ a: 404 }), ['a'], ['a'])).toEqual([]);
  });

  test('an error keeps the last list for that station and adds nothing new', async () => {
    const get = answers({ a: 'error', b: 'error', c: 'ok' });
    expect(await probeStations(get, ['a', 'b', 'c'], ['a'])).toEqual(['a', 'c']);
  });

  test('a body that is not a palette is dropped', async () => {
    const get = async () => ({ mode: 'dark', colors: { ...colors, red: 'x' } });
    expect(await probeStations(get, ['a'], ['a'])).toEqual([]);
  });

  test('a station that left the snapshot leaves the list', async () => {
    expect(await probeStations(answers({ a: 'ok' }), ['a'], ['a', 'gone'])).toEqual(['a']);
  });
});

describe('the stored choice', () => {
  const memory = () => {
    const data = new Map<string, string>();
    return {
      data,
      getItem: (k: string) => data.get(k) ?? null,
      setItem: (k: string, v: string) => void data.set(k, v),
      removeItem: (k: string) => void data.delete(k),
    };
  };
  const blocked = {
    getItem: () => {
      throw new Error('blocked');
    },
    setItem: () => {
      throw new Error('blocked');
    },
    removeItem: () => {
      throw new Error('blocked');
    },
  };

  test('write and read a station id; off removes it', () => {
    const store = memory();
    expect(readOmarchyChoice(store)).toBeNull();
    writeOmarchyChoice(store, 'st-1');
    expect(store.data.get(OMARCHY_KEY)).toBe('st-1');
    expect(OMARCHY_KEY).toBe('fd-omarchy-theme');
    expect(readOmarchyChoice(store)).toBe('st-1');
    writeOmarchyChoice(store, null);
    expect(readOmarchyChoice(store)).toBeNull();
  });

  test('a throwing storage does not throw and reads as off', () => {
    expect(readOmarchyChoice(blocked)).toBeNull();
    expect(() => writeOmarchyChoice(blocked, 'st-1')).not.toThrow();
    expect(() => writeOmarchyChoice(blocked, null)).not.toThrow();
    expect(readOmarchyChoice(null)).toBeNull();
  });
});

describe('the select', () => {
  const stations = [station('station-a'), station('station-b')];
  const getPalette = async () => body;

  test('nothing shows while the choice is off and no station has a palette', () => {
    setOmarchyChoice(null);
    expect(
      renderToStaticMarkup(<OmarchyThemeSelect stations={stations} getPalette={getPalette} />),
    ).toBe('');
  });

  test('a chosen station stays in the list with Off, with the label as aria-label and title', () => {
    setOmarchyChoice('station-b');
    const html = renderToStaticMarkup(
      <OmarchyThemeSelect stations={stations} getPalette={getPalette} />,
    );
    expect(html).toContain(`aria-label="${OMARCHY_LABEL}"`);
    expect(html).toContain(`title="${OMARCHY_LABEL}"`);
    expect(html).toContain('<option value="">Off</option>');
    expect(html).toContain('<option value="station-b" selected="">Beta</option>');
    expect(html).not.toContain('Alpha');
    setOmarchyChoice(null);
  });

  test('the Prefs row has the select only with the owner call', () => {
    setOmarchyChoice('station-a');
    const without = renderToStaticMarkup(<PrefsRow stations={stations} />);
    expect(without).not.toContain('<select');
    const owner = renderToStaticMarkup(<PrefsRow stations={stations} getPalette={getPalette} />);
    expect(owner).toContain('<select');
    expect(owner).toContain('aria-label="Theme"');
    setOmarchyChoice(null);
  });
});
