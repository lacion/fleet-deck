import { describe, expect, test } from 'bun:test';
import { DeckError, deckHttp } from '../settings/http';
import { CAPTURE_ID_HEADER, captureActions } from './capture';

type Seen = { url: string; init: RequestInit };

function http(answer: (url: string, init: RequestInit) => Response) {
  const seen: Seen[] = [];
  const io = deckHttp(
    async (url, init) => {
      seen.push({ url: String(url), init: init ?? {} });
      return answer(String(url), init ?? {});
    },
    () => 'csrf-1',
    1000,
  );
  return { io, seen };
}

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });

describe('the deck transport (R13 P5a)', () => {
  test('post passes the extra headers and keeps the CSRF token', async () => {
    const { io, seen } = http(() => json({}));
    await io.post('/x', { a: 1 }, { 'X-Extra': 'e' });
    const headers = seen[0]?.init.headers as Record<string, string>;
    expect(seen[0]?.init.method).toBe('POST');
    expect(headers['X-Extra']).toBe('e');
    expect(headers['X-CSRF-Token']).toBe('csrf-1');
    expect(headers['Content-Type']).toBe('application/json');
  });

  test('del sends DELETE with no body and the extra headers', async () => {
    const { io, seen } = http(() => json({ state: 'deleted' }));
    await io.del('/api/artifacts/s/a', { 'X-FleetDeck-Elevation': 'tok' });
    const headers = seen[0]?.init.headers as Record<string, string>;
    expect(seen[0]?.init.method).toBe('DELETE');
    expect(seen[0]?.init.body).toBeNull();
    expect(headers['X-FleetDeck-Elevation']).toBe('tok');
    expect(headers['X-CSRF-Token']).toBe('csrf-1');
    expect(headers['Content-Type']).toBeUndefined();
  });

  test('get still sends GET without a body', async () => {
    const { io, seen } = http(() => json({}));
    await io.get('/x');
    expect(seen[0]?.init.method).toBe('GET');
    expect(seen[0]?.init.body).toBeNull();
  });
});

describe('captureActions', () => {
  const published = { artifact: { artifact_id: 'a1' }, version: { version: 1 }, created: true };
  const make = (answer: (url: string, init: RequestInit) => Response) => {
    const { io, seen } = http(answer);
    return { actions: captureActions(io, io), seen };
  };

  test('take posts the action with the capture ID header to the station name', async () => {
    const { actions, seen } = make(() =>
      json({ address: null, info: { monitors: [] }, capture: { capture_id: 'C1', published } }),
    );
    const r = await actions.take('my station', 'DP-1', 'C1');
    expect(r.kind).toBe('done');
    expect(seen[0]?.url).toBe('/api/stations/my%20station/desktop/act');
    expect(JSON.parse(String(seen[0]?.init.body))).toEqual({
      action: 'screen_capture',
      monitor: 'DP-1',
    });
    const headers = (seen[0]?.init.headers ?? {}) as Record<string, string>;
    expect(headers[CAPTURE_ID_HEADER]).toBe('C1');
  });

  test('take reads a 202 and a deleted body', async () => {
    const pending = make(() => json({ capture_id: 'C1', state: 'pending' }, 202));
    expect(await pending.actions.take('s', 'DP-1', 'C1')).toEqual({
      kind: 'pending',
      captureId: 'C1',
    });
    const unknown = make(() => json({ capture_id: 'C1', state: 'unknown' }, 202));
    expect((await unknown.actions.take('s', 'DP-1', 'C1')).kind).toBe('unknown');
    const deleted = make(() => json({ capture_id: 'C1', state: 'deleted' }));
    expect((await deleted.actions.take('s', 'DP-1', 'C1')).kind).toBe('deleted');
  });

  test('take throws the deck error and an invalid answer', async () => {
    const err = make(() => json({ error: 'busy', message: 'A capture is already running.' }, 409));
    const e = await err.actions.take('s', 'DP-1', 'C1').catch((x: unknown) => x);
    expect(e).toBeInstanceOf(DeckError);
    expect((e as DeckError).message).toBe('A capture is already running.');
    const bad = make(() => json({ nope: true }));
    await expect(bad.actions.take('s', 'DP-1', 'C1')).rejects.toThrow('invalid');
  });

  test('status reads the lookup route', async () => {
    const { actions, seen } = make(() => json({ state: 'pending', capture_id: 'C1' }));
    expect((await actions.status('s', 'C1')).state).toBe('pending');
    expect(seen[0]?.url).toBe('/api/stations/s/desktop/captures/C1');
    const bad = make(() => json({ state: 'weird' }));
    await expect(bad.actions.status('s', 'C1')).rejects.toThrow('invalid');
  });

  test('features lists the features; a 404 is null; another failure is thrown', async () => {
    const ok = make(() => json({ features: ['desktop_capture.v1', 'x'] }));
    expect(await ok.actions.features()).toEqual(['desktop_capture.v1', 'x']);
    const old = make(() => json({ error: 'not_found', message: 'no' }, 404));
    expect(await old.actions.features()).toBeNull();
    const down = make(() => json({ error: 'owner_only', message: 'no' }, 403));
    await expect(down.actions.features()).rejects.toBeInstanceOf(DeckError);
  });

  test('record posts screen_record with the seconds and the ID header, with the time limit', async () => {
    const { io, seen } = http(() =>
      json({ address: null, info: { monitors: [] }, capture: { capture_id: 'C1', published } }),
    );
    const limits: number[] = [];
    const actions = captureActions(io, io, (ms) => {
      limits.push(ms);
      return io;
    });
    const r = await actions.record('my station', 'DP-1', 30, 'C1');
    expect(r.kind).toBe('done');
    expect(limits).toEqual([75_000]);
    expect(seen[0]?.url).toBe('/api/stations/my%20station/desktop/act');
    expect(JSON.parse(String(seen[0]?.init.body))).toEqual({
      action: 'screen_record',
      monitor: 'DP-1',
      seconds: 30,
    });
    const headers = (seen[0]?.init.headers ?? {}) as Record<string, string>;
    expect(headers[CAPTURE_ID_HEADER]).toBe('C1');
  });

  test('record without a transport for the time limit uses the capture transport', async () => {
    const { actions, seen } = make(() => json({ capture_id: 'C1', state: 'pending' }, 202));
    expect(await actions.record('s', 'DP-1', 5, 'C1')).toEqual({
      kind: 'pending',
      captureId: 'C1',
    });
    expect(seen.length).toBe(1);
  });

  test('status keeps a valid recording progress and drops an invalid one', async () => {
    const good = make(() =>
      json({
        state: 'pending',
        capture_id: 'C1',
        recording: { phase: 'recording', requested_seconds: 30, started_at: 1, elapsed_ms: 2 },
      }),
    );
    const s = await good.actions.status('s', 'C1');
    expect(s.recording?.phase).toBe('recording');
    const bad = make(() =>
      json({ state: 'pending', capture_id: 'C1', recording: { phase: 'dancing' } }),
    );
    expect((await bad.actions.status('s', 'C1')).recording).toBeUndefined();
  });

  test('active reads idle and active, and rejects another body', async () => {
    const idle = make(() => json({ state: 'idle' }));
    expect(await idle.actions.active('my station')).toEqual({ state: 'idle' });
    expect(idle.seen[0]?.url).toBe('/api/stations/my%20station/desktop/recording');
    expect(idle.seen[0]?.init.method).toBe('GET');
    const live = make(() =>
      json({
        state: 'active',
        capture_id: 'C1',
        status: { state: 'pending', capture_id: 'C1' },
      }),
    );
    const a = await live.actions.active('s');
    expect(a.state === 'active' && a.capture_id).toBe('C1');
    const bad = make(() => json({ state: 'active' }));
    await expect(bad.actions.active('s')).rejects.toThrow('invalid');
  });

  test('stop posts an empty object to the stop route of the capture', async () => {
    const { actions, seen } = make(() =>
      json({ state: 'failed', capture_id: 'C1', error: 'e', message: 'm' }),
    );
    const s = await actions.stop('my station', 'C 1');
    expect(s.state).toBe('failed');
    expect(seen[0]?.url).toBe('/api/stations/my%20station/desktop/captures/C%201/stop');
    expect(seen[0]?.init.method).toBe('POST');
    expect(JSON.parse(String(seen[0]?.init.body))).toEqual({});
  });
});
