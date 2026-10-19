// BG-3 (design `docs/v2/design/board-gaps.md` 4.4 to 4.6): the calls and the fixed texts of the
// setup reports from unpaired keys. A report is self-reported by a key that is not paired: the
// board never shows it as a station, a paired badge or a grant. The code texts are fixed here;
// peer text never supplies them. All routes are owner only.
import { DeckError, type DeckHttp } from '../settings/http';

/** The retry permission of a report: `queued` until the station fetches it. */
export type RetryView = { state: 'queued' | 'delivered'; expires_at: number };

export type SetupReport = {
  /** 64 lowercase hex characters. */
  fingerprint: string;
  /** Self-reported; rendered as plain text. */
  name: string;
  code: string;
  phase: string;
  /** Unix ms, the time of the deck. */
  received_at: number;
  /** Unix ms. */
  last_contact: number;
  terminal_id: string;
  state_digest: string;
  retry: RetryView | null;
};

/** The reporting window; `deadline_ms` is null while it is closed. */
export type ReportsWindow = { open: boolean; deadline_ms: number | null };

export type ReportsView = { window: ReportsWindow; reports: SetupReport[] };

export type ReportsActions = {
  list(): Promise<ReportsView>;
  setWindow(open: boolean, elevation: string | null): Promise<ReportsWindow>;
  allowRetry(
    fingerprint: string,
    terminalId: string,
    stateDigest: string,
    requestId: string,
    elevation: string | null,
  ): Promise<RetryView>;
  cancelRetry(fingerprint: string): Promise<void>;
  dismiss(fingerprint: string): Promise<void>;
};

/** The text for an answer that does not have the shape. */
export const INVALID_ANSWER = 'The deck sent an invalid answer.';

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

const isString = (v: unknown): v is string => typeof v === 'string';
const isNumber = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v);

export function isWindow(v: unknown): v is ReportsWindow {
  return (
    record(v) &&
    typeof v['open'] === 'boolean' &&
    (v['deadline_ms'] === null || isNumber(v['deadline_ms']))
  );
}

export function isRetry(v: unknown): v is RetryView {
  return (
    record(v) &&
    (v['state'] === 'queued' || v['state'] === 'delivered') &&
    isNumber(v['expires_at'])
  );
}

export function isReport(v: unknown): v is SetupReport {
  return (
    record(v) &&
    isString(v['fingerprint']) &&
    isString(v['name']) &&
    isString(v['code']) &&
    isString(v['phase']) &&
    isNumber(v['received_at']) &&
    isNumber(v['last_contact']) &&
    isString(v['terminal_id']) &&
    isString(v['state_digest']) &&
    (v['retry'] === null || isRetry(v['retry']))
  );
}

function invalid(): never {
  throw new Error(INVALID_ANSWER);
}

const seg = encodeURIComponent;

const BASE = '/api/provision-reports';

/** The setup report calls over a transport. */
export function reportsActions(io: DeckHttp): ReportsActions {
  return {
    list: async () => {
      const v = await io.get(BASE);
      if (!record(v) || !isWindow(v['window'])) return invalid();
      const rows = v['reports'];
      if (!Array.isArray(rows) || !rows.every(isReport)) return invalid();
      return { window: v['window'], reports: rows };
    },
    setWindow: async (open, elevation) => {
      const body = elevation === null ? { open } : { open, elevation };
      const v = await io.post(`${BASE}/window`, body);
      return isWindow(v) ? v : invalid();
    },
    allowRetry: async (fingerprint, terminalId, stateDigest, requestId, elevation) => {
      const body = {
        terminal_id: terminalId,
        state_digest: stateDigest,
        request_id: requestId,
        ...(elevation === null ? {} : { elevation }),
      };
      const v = await io.post(`${BASE}/${seg(fingerprint)}/retry`, body);
      return record(v) && isRetry(v['retry']) ? v['retry'] : invalid();
    },
    cancelRetry: async (fingerprint) => {
      await io.del(`${BASE}/${seg(fingerprint)}/retry`);
    },
    dismiss: async (fingerprint) => {
      await io.del(`${BASE}/${seg(fingerprint)}`);
    },
  };
}

/** True when the deck has no pairing: the section is hidden. */
export const isUnavailable = (e: unknown): boolean =>
  e instanceof DeckError && e.status === 503 && e.code === 'pairing_unavailable';

export const REPORT_CHANGED_TEXT =
  'The reported setup condition changed. Review the latest report.';

/** The text for a failed retry, by the deck's error code. */
export function retryError(code: string | null, message: string): string {
  return code === 'report_changed' ? REPORT_CHANGED_TEXT : message;
}

export const DISABLED_TEXT =
  'Retrying cannot repair this condition. Fleet Deck has no board repair for it yet.';
export const QUEUED_TEXT =
  'Retry allowed. The setup supervisor will check again within about ten minutes while it is running.';
export const DELIVERED_TEXT = 'The retry was sent to the station.';

export type CodeInfo = { readonly text: string; readonly retry: boolean };

const allowed = (text: string): CodeInfo => ({ text, retry: true });
const disabled = (text: string): CodeInfo => ({ text, retry: false });

/** Design 4.5: the owner text of each code, and whether one setup retry can be allowed. */
export const CODE_TEXTS: Readonly<Record<string, CodeInfo>> = {
  file_invalid: disabled('The provisioning file is missing, unsafe, or invalid.'),
  release_untrusted: disabled(
    'The trusted manifest or an installed hook file is missing, is not owned by root, or does not match its hash.',
  ),
  build_mismatch: disabled(
    'The running build or the binary of the station does not match the trusted manifest.',
  ),
  config_conflict: disabled('An existing station configuration conflicts with this setup.'),
  config_changed: disabled('The provisioning file changed after the pairing started.'),
  service_conflict: disabled(
    'The station service that is installed has another binary or another data folder.',
  ),
  station_identity_changed: disabled(
    'The station key is not the key that the bootstrap saved first.',
  ),
  paired_elsewhere: disabled(
    'The station is already paired with a deck that has another fingerprint.',
  ),
  denied: allowed('The pairing request was refused.'),
  cancelled: allowed('The pairing request was cancelled.'),
  revoked: allowed('The pairing request or authorization was revoked.'),
  fingerprint_mismatch: disabled('The peer did not match the configured deck fingerprint.'),
  request_id_conflict: disabled(
    'The pairing request identifier conflicts with an earlier request.',
  ),
  pending: allowed('An earlier pairing request is still pending.'),
  unsupported: disabled('A required setup operation is not supported by this build.'),
  invalid_request: disabled('A setup request was rejected as invalid.'),
  retry_file: disabled('Fleet Deck could not safely use the saved pairing recovery file.'),
  pairing_failed: allowed('Pairing did not finish.'),
  provider_probe_failed: allowed(
    'A configured provider is broken, unknown, or has no readable version.',
  ),
  integration_failed: allowed(
    'The Omarchy integration install or its status check did not succeed.',
  ),
  unknown_error: allowed('Setup stopped for an unrecognized reason.'),
};

const UNKNOWN: CodeInfo = allowed('Setup stopped for an unrecognized reason.');

/** The text of a code. A code outside the table is shown as `unknown_error` (design 4.6). */
export function codeInfo(code: string): CodeInfo {
  return (Object.hasOwn(CODE_TEXTS, code) ? CODE_TEXTS[code] : undefined) ?? UNKNOWN;
}

/** What the row offers: a retry that exists shows its state; else allow or the disabled text. */
export type RowControl =
  | { readonly kind: 'allow' }
  | { readonly kind: 'disabled' }
  | { readonly kind: 'queued' }
  | { readonly kind: 'delivered' };

export function rowControl(report: SetupReport): RowControl {
  if (report.retry !== null) return { kind: report.retry.state };
  return codeInfo(report.code).retry ? { kind: 'allow' } : { kind: 'disabled' };
}

/** The first 8 characters of the fingerprint, for the label of a row. */
export const shortKey = (fingerprint: string): string => fingerprint.slice(0, 8);

/** A new request ID: 32 hex characters. */
export function newRequestId(): string {
  if (typeof crypto.randomUUID === 'function') return crypto.randomUUID().replaceAll('-', '');
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
}

/** The key of one report state: the same pair keeps the same request ID. */
const idKey = (r: SetupReport): string => `${r.fingerprint}:${r.terminal_id}:${r.state_digest}`;

/** The request ID for the row and its terminal/digest pair; it is made once and kept. */
export function requestIdFor(
  ids: Map<string, string>,
  report: SetupReport,
  make: () => string = newRequestId,
): string {
  const key = idKey(report);
  const held = ids.get(key);
  if (held !== undefined) return held;
  const fresh = make();
  ids.set(key, fresh);
  return fresh;
}

/** Drops the request IDs of pairs that are no longer shown. */
export function pruneRequestIds(ids: Map<string, string>, reports: readonly SetupReport[]): void {
  const shown = new Set(reports.map(idKey));
  for (const key of [...ids.keys()]) if (!shown.has(key)) ids.delete(key);
}
