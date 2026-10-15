// DL-146: "Add a device". One POST makes a single-use login link for a new device.
import type { DeviceLink, DeviceLinkEntry } from '../gen';

type Post = (path: string, body: unknown) => Promise<unknown>;

export type DeviceLinkResult =
  | { readonly ok: true; readonly link: DeviceLink }
  | { readonly ok: false; readonly text: string };

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

const TRUSTS: readonly unknown[] = ['trusted', 'self_signed', 'unknown'];

const entry = (v: unknown): v is DeviceLinkEntry =>
  record(v) &&
  typeof v['label'] === 'string' &&
  typeof v['url'] === 'string' &&
  typeof v['certificate_sha256'] === 'string' &&
  TRUSTS.includes(v['trust']);

/** `POST /api/auth/device-link`: the deck answers with one link for each board address. */
export function deviceLinkAction(post: Post): () => Promise<DeviceLinkResult> {
  return async () => {
    try {
      const reply = await post('/api/auth/device-link', {});
      if (
        !record(reply) ||
        !Array.isArray(reply['links']) ||
        reply['links'].length === 0 ||
        !reply['links'].every(entry) ||
        typeof reply['expires_at'] !== 'number' ||
        typeof reply['certificate_sha256'] !== 'string' ||
        typeof reply['self_signed'] !== 'boolean'
      ) {
        return { ok: false, text: 'The deck gave an answer that the board cannot read.' };
      }
      return { ok: true, link: reply as DeviceLink };
    } catch (error) {
      return { ok: false, text: error instanceof Error ? error.message : 'The request failed.' };
    }
  };
}

/** The first 16 hex characters of a certificate fingerprint: groups of 4, upper case, spaced. */
export function fingerprintGroups(sha: string): string {
  const head = sha.replaceAll(':', '').slice(0, 16).toUpperCase();
  return head.match(/.{1,4}/g)?.join(' ') ?? '';
}

/**
 * The first link whose certificate browsers trust (DL-190); else the first link whose label is
 * not "This computer" (a phone cannot reach it); else the first.
 */
export function defaultLinkIndex(links: readonly DeviceLinkEntry[]): number {
  const trusted = links.findIndex((l) => l.trust === 'trusted');
  if (trusted !== -1) return trusted;
  const i = links.findIndex((l) => l.label !== 'This computer');
  return i === -1 ? 0 : i;
}

/** The host name of a login link, without port and brackets; empty when the link is no URL. */
function hostOf(url: string): string {
  try {
    return new URL(url).hostname;
  } catch {
    return '';
  }
}

/** A self-signed tailnet name: the trusted certificate from Tailscale would fix it. */
export function needsTrustedCertificate(entry: DeviceLinkEntry): boolean {
  return entry.trust === 'self_signed' && hostOf(entry.url).toLowerCase().endsWith('.ts.net');
}

/** `m:ss` of the time that is left; a negative or an invalid time is `0:00`. */
export function countdown(ms: number): string {
  const total = Number.isFinite(ms) && ms > 0 ? Math.ceil(ms / 1000) : 0;
  const s = total % 60;
  return `${Math.floor(total / 60)}:${s < 10 ? '0' : ''}${s}`;
}
