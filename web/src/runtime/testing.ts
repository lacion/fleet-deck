import type { BoardClientMessage, BoardServerMessage, DeckSession, EventEnvelope } from '../gen';
import { summary } from '../testing/summary';
import type { Environment, Socket } from './connection';

export const loginView = {
  actor: 'owner',
  scopes: ['read', 'drive', 'approve', 'admin'],
  csrf: 'test-csrf',
  expires_at: 999_999,
};
export class FakeSocket implements Socket {
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: ((event: { code: number }) => void) | null = null;
  onerror: (() => void) | null = null;
  sent: BoardClientMessage[] = [];
  closed = false;
  send(text: string) {
    this.sent.push(JSON.parse(text));
  }
  close() {
    this.closed = true;
    this.onclose?.({ code: 1000 });
  }
  open() {
    this.onopen?.();
  }
  message(message: BoardServerMessage) {
    this.onmessage?.({ data: JSON.stringify(message) });
  }
  drop(code = 1006) {
    this.onclose?.({ code });
  }
  live(version = 1, sessions: DeckSession[] = [card()]) {
    this.open();
    this.message({ type: 'hello', protocol: 1, deck_incarnation: 'deck' });
    this.message({ type: 'snapshot', version, stations: [], sessions });
  }
}
export class FakeEnvironment implements Environment {
  clock = 0;
  private next = 0;
  timers = new Map<number, { at: number; fn: () => void }>();
  sockets: FakeSocket[] = [];
  requests: { path: string; init: RequestInit }[] = [];
  reply: (path: string, init: RequestInit) => Promise<Response> = async () =>
    Response.json(loginView);
  fetch = (path: string, init: RequestInit): Promise<Response> => {
    this.requests.push({ path, init });
    return this.reply(path, init);
  };
  socket = (): FakeSocket => {
    const socket = new FakeSocket();
    this.sockets.push(socket);
    return socket;
  };
  random = () => 1;
  schedule = (fn: () => void, ms: number): (() => void) => {
    const id = this.next++;
    this.timers.set(id, { fn, at: this.clock + ms });
    return () => {
      this.timers.delete(id);
    };
  };
  advance(ms: number) {
    const target = this.clock + ms;
    for (;;) {
      const next = [...this.timers.entries()].sort((a, b) => a[1].at - b[1].at)[0];
      if (!next || next[1].at > target) break;
      this.clock = next[1].at;
      this.timers.delete(next[0]);
      next[1].fn();
    }
    this.clock = target;
  }
  latest(): FakeSocket {
    const socket = this.sockets.at(-1);
    if (!socket) throw new Error('No fake socket');
    return socket;
  }
}
export async function settle() {
  for (let i = 0; i < 12; i++) await Promise.resolve();
}
export const key = { station_id: 'station', session_id: 'session' };
export function card(title = 'initial'): DeckSession {
  return {
    station_id: key.station_id,
    summary: summary(key.session_id, 'idle', 1, { title }),
    status: 'idle',
  };
}
export function envelope(seq: number, text = 'hello'): EventEnvelope {
  return {
    station_seq: seq,
    session_id: key.session_id,
    session_seq: seq,
    at: 1,
    proto_version: 1,
    raw_ref: null,
    event: { kind: 'message.completed', turn_id: 'turn', parent_node_id: null, text },
  };
}
