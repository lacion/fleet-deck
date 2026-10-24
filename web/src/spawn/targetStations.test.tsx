import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { station } from '../testing/fleet';
import { fleetElevation, grantFields, NO_FLEET } from './fleetControl';
import { TargetStations, targetChoices } from './TargetStations';

const stations = [
  station('station-a'),
  station('station-b'),
  station('station-c', { name: 'Gamma' }),
];

describe('the target stations of a coordinator grant (ADR-006 Creation)', () => {
  test('the choices are every paired station except the session own station', () => {
    expect(targetChoices(stations, 'station-a').map((s) => s.station_id)).toEqual([
      'station-b',
      'station-c',
    ]);
    expect(targetChoices(stations, 'station-c').map((s) => s.station_id)).toEqual([
      'station-a',
      'station-b',
    ]);
  });

  test('the section lists this station as always included, then the others, none picked', () => {
    const html = renderToStaticMarkup(
      <TargetStations stations={stations} ownId="station-a" picked={[]} onChange={() => {}} />,
    );
    expect(html).toContain('Grant stations');
    expect(html).toContain('This station (always)');
    expect(html).toContain('Beta');
    expect(html).toContain('Gamma');
    // The own station has only the fixed row; nothing else is checked.
    expect(html.match(/checked=""/g)?.length).toBe(1);
    expect(html).toContain(
      'This one list bounds every class of the grant: the coordinator can act on sessions, desktops and screenshots of these stations only. Each one needs a current link with coordinator v2.',
    );
  });

  test('a picked station shows as checked', () => {
    const html = renderToStaticMarkup(
      <TargetStations
        stations={stations}
        ownId="station-a"
        picked={['station-c']}
        onChange={() => {}}
      />,
    );
    expect(html.match(/checked=""/g)?.length).toBe(2);
  });

  test('the spec carries the targets only when some are picked', () => {
    expect(grantFields(NO_FLEET, [])).toEqual({ classes: ['read', 'mail', 'drive'] });
    expect(grantFields(NO_FLEET)).toEqual({ classes: ['read', 'mail', 'drive'] });
    expect(grantFields(NO_FLEET, ['station-b', 'station-c'])).toEqual({
      classes: ['read', 'mail', 'drive'],
      stations: ['station-b', 'station-c'],
    });
  });

  test('targets alone ask for the owner elevation with the Fleet control label', async () => {
    const calls: (readonly string[] | undefined)[] = [];
    const ask = async (labels?: readonly string[]) => {
      calls.push(labels);
      return 'code-1';
    };
    const gate = { coordinator: true, importing: false, fleet: NO_FLEET };
    expect(await fleetElevation({ ...gate, targets: ['station-b'] }, ask)).toEqual({
      ok: true,
      elevation: 'code-1',
    });
    expect(calls).toEqual([['Fleet control']]);
    // No targets, no switch: no dialog. An import never asks.
    expect(await fleetElevation({ ...gate, targets: [] }, ask)).toEqual({
      ok: true,
      elevation: undefined,
    });
    expect(await fleetElevation({ ...gate, importing: true, targets: ['station-b'] }, ask)).toEqual(
      { ok: true, elevation: undefined },
    );
    expect(calls.length).toBe(1);
  });
});
