import type { BoardServerMessage } from '../gen';

type ObjectValue = Record<string, unknown>;
const object = (v: unknown): v is ObjectValue =>
  typeof v === 'object' && v !== null && !Array.isArray(v);
const text = (v: unknown): v is string => typeof v === 'string';
const sequence = (v: unknown) => typeof v === 'number' && Number.isSafeInteger(v) && v >= 0;
const optionalText = (v: unknown) => v === null || text(v);
const key = (v: ObjectValue) => text(v['station_id']) && text(v['session_id']);
const list = (v: unknown, check: (item: unknown) => boolean) =>
  Array.isArray(v) && v['every'](check);
const station = (v: unknown) =>
  object(v) &&
  text(v['station_id']) &&
  text(v['name']) &&
  typeof v['reachable'] === 'boolean' &&
  sequence(v['cursor']);
const card = (v: unknown) =>
  object(v) &&
  text(v['station_id']) &&
  object(v['summary']) &&
  text(v['summary']['session_id']) &&
  text(v['status']) &&
  ['queued', 'working', 'verifying', 'needs_you', 'idle', 'ended', 'unreachable', 'lost'].includes(
    v['status'],
  ) &&
  text(v['summary']['activity']) &&
  ['working', 'needs_you', 'background', 'idle', 'ended'].includes(v['summary']['activity']) &&
  optionalText(v['summary']['title']) &&
  optionalText(v['summary']['model']) &&
  (v['summary']['provider'] === null ||
    v['summary']['provider'] === 'claude' ||
    v['summary']['provider'] === 'codex');
const envelope = (v: unknown) =>
  object(v) &&
  sequence(v['station_seq']) &&
  sequence(v['session_seq']) &&
  text(v['session_id']) &&
  object(v['event']) &&
  text(v['event']['kind']);

const displayEvent = (v: unknown) =>
  object(v) &&
  sequence(v['station_seq']) &&
  sequence(v['session_seq']) &&
  typeof v['at'] === 'number' &&
  object(v['item']) &&
  text(v['item']['item']);

/** Validate the fields this runtime reads. Unknown event kinds remain readable (L16). */
export function boardMessage(raw: string): BoardServerMessage {
  const v: unknown = JSON.parse(raw);
  if (!object(v)) throw new Error('Invalid board message');
  let valid = false;
  switch (v['type']) {
    case 'hello':
      valid = v['protocol'] === 1 && text(v['deck_incarnation']);
      break;
    case 'snapshot':
      valid = sequence(v['version']) && list(v['stations'], station) && list(v['sessions'], card);
      break;
    case 'delta':
      valid =
        sequence(v['version']) &&
        (v['stations'] === null || list(v['stations'], station)) &&
        list(v['sessions'], card) &&
        list(v['removed'], (item) => object(item) && key(item));
      break;
    case 'timeline_start':
      valid = key(v) && optionalText(v['incarnation']) && typeof v['reset'] === 'boolean';
      break;
    case 'timeline_events':
      valid = key(v) && list(v['events'], envelope);
      break;
    // The timeline frame of a wall display (R13 §5D item 7).
    case 'display_timeline_events':
      valid = key(v) && list(v['events'], displayEvent) && sequence(v['high_water']);
      break;
    // The people features of a shared session (R11, DL-166).
    case 'presence':
      valid = key(v) && list(v['who'], text);
      break;
    case 'chat': {
      const m = v['message'];
      valid =
        key(v) &&
        object(m) &&
        sequence(m['seq']) &&
        text(m['author']) &&
        text(m['text']) &&
        typeof m['at'] === 'number';
      break;
    }
    case 'driver':
      valid = key(v) && optionalText(v['driver']);
      break;
    case 'drive_request':
      valid = key(v) && list(v['requests'], text);
      break;
    // The wall control relay (B17): only a display socket that declared `wall_control.v1` gets it.
    case 'wall_control': {
      const f = v['frame'];
      valid =
        object(f) &&
        text(f['incarnation']) &&
        sequence(f['seq']) &&
        sequence(f['generation']) &&
        object(f['event']) &&
        text(f['event']['kind']) &&
        object(f['state']);
      break;
    }
    case 'error':
      valid = text(v['code']) && text(v['message']);
      break;
    default:
      throw new Error('Invalid board message');
  }
  if (!valid) throw new Error('Invalid board message');
  // SAFETY: the checks above validate every field the runtime accesses; generated
  // contracts describe the other canonical event/projection fields sent by the deck.
  return v as BoardServerMessage;
}
