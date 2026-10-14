// DL-178: the tailnet and pairing-window calls of the board. The shapes are the deck's
// (`GET /api/tailnet`, `/api/tailnet/devices`, `/api/pairing/*`); all routes are owner only.
import type { DeckIo } from '../settings/http';

/** The state of "Use Tailscale automatically" (tailnet design 14.4). */
export type AutoView = {
  /** The saved setting `tailnet_auto`. */
  enabled: boolean;
  /** Why the mode did not apply at start (only when it was on at start). */
  start_reason?: string;
  /** The automatic addresses that were not used at start. */
  skipped: { address: string; reason: string }[];
  /** The usable tailnet addresses differ from the automatic entries: restart to use them. */
  changed: boolean;
};

export type TailnetView = {
  available: boolean;
  /** `windows_tailscale` when only the Windows program exists; else a text of the deck. */
  reason?: string;
  /** The Windows program of WSL that the owner can choose (with `windows_tailscale`). */
  windows_program?: string;
  self?: { dns_name: string; ips: string[]; login?: string; os?: string };
  userspace: boolean;
  /** What "Use tailnet" would save now. */
  managed: { listen: string[]; link_listen: string[]; tailnet_hostname?: string };
  /** The saved deck.toml values. */
  current: { listen: string[]; link_listen: string[] };
  auto: AutoView;
};

export type UseAnswer = {
  saved: boolean;
  restart_needed: boolean;
  listen: string[];
  link_listen: string[];
  tailnet_hostname?: string | null;
};

export type Device = {
  host_name: string;
  dns_name: string;
  ips: string[];
  os?: string;
  login?: string;
  tags: string[];
  online: boolean;
  state: 'paired' | 'request waiting' | 'none';
  /** Set only while the window is open and the device is not paired. */
  command: string | null;
};

export type DevicesView = { available: boolean; reason?: string; devices: Device[] };

export type WindowView = { open: boolean; generation: number; deadline_ms: number | null };

export type PairRequest = {
  request_id: string;
  requested_name: string;
  /** 64 hex characters. */
  station_fingerprint: string;
  offered_scopes: string[];
  source_ip: string;
  admitted_at: number;
  expires_at: number;
  /** 64 hex characters. */
  digest: string;
  tailscale: null | {
    host_name?: string;
    login?: string;
    tags: string[];
    shared?: boolean;
    error?: string;
  };
};

export type RequestsView = { window: WindowView; requests: PairRequest[] };

export type TailnetActions = {
  view(): Promise<TailnetView>;
  use(elevation: string | null): Promise<UseAnswer>;
  devices(): Promise<DevicesView>;
  window(): Promise<WindowView>;
  setWindow(open: boolean, elevation: string | null): Promise<WindowView>;
  requests(): Promise<RequestsView>;
  approve(id: string, fingerprint: string, digest: string, elevation: string | null): Promise<void>;
  deny(id: string): Promise<void>;
};

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

function shaped<T>(v: unknown, fields: readonly string[], what: string): T {
  if (!record(v) || fields.some((f) => !(f in v))) {
    throw new Error(`The deck sent an invalid ${what}.`);
  }
  return v as T;
}

const seg = encodeURIComponent;

/** A body with the elevation only when there is one. */
const withElevation = (body: Record<string, unknown>, elevation: string | null) =>
  elevation === null ? body : { ...body, elevation };

/** The tailnet calls over a transport. */
export function tailnetActions(io: DeckIo): TailnetActions {
  return {
    view: async () =>
      shaped<TailnetView>(
        await io.get('/api/tailnet'),
        ['available', 'userspace', 'managed', 'current', 'auto'],
        'tailnet view',
      ),
    use: async (elevation) =>
      shaped<UseAnswer>(
        await io.post('/api/tailnet/use', withElevation({}, elevation)),
        ['saved', 'restart_needed'],
        'save answer',
      ),
    devices: async () =>
      shaped<DevicesView>(await io.get('/api/tailnet/devices'), ['devices'], 'device list'),
    window: async () =>
      shaped<WindowView>(
        await io.get('/api/pairing/window'),
        ['open', 'generation'],
        'pairing window',
      ),
    setWindow: async (open, elevation) =>
      shaped<WindowView>(
        await io.post('/api/pairing/window', withElevation({ open }, elevation)),
        ['open', 'generation'],
        'pairing window',
      ),
    requests: async () =>
      shaped<RequestsView>(
        await io.get('/api/pairing/requests'),
        ['window', 'requests'],
        'request list',
      ),
    approve: async (id, fingerprint, digest, elevation) => {
      await io.post(
        `/api/pairing/requests/${seg(id)}/approve`,
        withElevation({ fingerprint, digest }, elevation),
      );
    },
    deny: async (id) => {
      await io.post(`/api/pairing/requests/${seg(id)}/deny`, {});
    },
  };
}
