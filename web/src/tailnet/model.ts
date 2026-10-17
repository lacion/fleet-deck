// DL-178: the pure rules of the tailnet card and the pairing tab.
import type { AutoView, Device, PairRequest, TailnetView, WindowView } from './api';

/** Which button the Tailnet card shows: "Use tailnet", "Update", or none. */
export function useButton(view: TailnetView): 'use' | 'update' | 'none' {
  if (!view.available || view.userspace) return 'none';
  const listen = new Set(view.current.listen);
  const link = new Set(view.current.link_listen);
  const total = view.managed.listen.length + view.managed.link_listen.length;
  const present =
    view.managed.listen.filter((a) => listen.has(a)).length +
    view.managed.link_listen.filter((a) => link.has(a)).length;
  if (present === total) return 'none';
  return present === 0 ? 'use' : 'update';
}

/** True when the deck uses Tailscale for Windows from WSL (design 15.1). */
export const usesWindows = (view: TailnetView): boolean => view.program === 'windows';

/** The notes under the toggle: why it did not apply, the skipped addresses, and a change. */
export function autoNotes(auto: AutoView): string[] {
  const notes: string[] = [];
  if (auto.start_reason !== undefined) notes.push(`Not applied at start: ${auto.start_reason}`);
  for (const s of auto.skipped) notes.push(`${s.address} not used: ${s.reason}`);
  if (auto.changed) notes.push('Your tailnet address changed. Restart the deck to use it.');
  return notes;
}

/** The 64 hex characters in groups of 4 separated by one space. */
export function groupFingerprint(hex: string): string {
  return hex.replace(/(.{4})(?=.)/g, '$1 ');
}

/** `HH:MM` in the time zone of the browser. */
export function clockText(at: number): string {
  const d = new Date(at);
  const two = (n: number) => String(n).padStart(2, '0');
  return `${two(d.getHours())}:${two(d.getMinutes())}`;
}

/** The line about the pairing window. */
export function windowText(w: WindowView, now: number): string {
  if (!w.open) return 'The deck is not accepting pairing requests.';
  if (w.deadline_ms === null) return 'Accepting pairing requests.';
  const left = Math.max(0, Math.ceil((w.deadline_ms - now) / 60_000));
  return `Accepting pairing requests until ${clockText(w.deadline_ms)} (${left} min left).`;
}

/** The text for a failed approval, by the deck's error code. */
export function approveError(code: string | null, message: string): string {
  switch (code) {
    case 'name_taken':
      return 'Another station has this name. Deny the request and ask for another name.';
    case 'already_paired':
      return 'This station key is already paired.';
    case 'window_closed':
      return 'The window closed. Open it again and ask the station to retry.';
    case 'digest_mismatch':
    case 'fingerprint_mismatch':
      return 'The request changed. Check the fingerprint again.';
    default:
      return message;
  }
}

export type DeviceGroups = {
  readonly addable: Device[];
  readonly waiting: Device[];
  readonly paired: Device[];
  readonly offline: Device[];
  readonly unsupported: Device[];
};

const UNSUPPORTED_OS = new Set(['ios', 'ipados', 'android', 'tvos']);

/** The groups of the device list, in input order. The first rule that matches wins. */
export function groupDevices(devices: readonly Device[]): DeviceGroups {
  const groups: DeviceGroups = {
    addable: [],
    waiting: [],
    paired: [],
    offline: [],
    unsupported: [],
  };
  for (const d of devices) {
    if (d.state === 'paired') groups.paired.push(d);
    else if (d.state === 'request waiting') groups.waiting.push(d);
    else if (UNSUPPORTED_OS.has((d.os ?? '').toLowerCase())) groups.unsupported.push(d);
    else if (!d.online) groups.offline.push(d);
    else groups.addable.push(d);
  }
  return groups;
}

const OS_LABELS: Readonly<Record<string, string>> = {
  linux: 'Linux',
  macos: 'macOS',
  windows: 'Windows',
  ios: 'iOS',
  ipados: 'iPadOS',
  android: 'Android',
};

/** The name of an operating system for a person. */
export function osLabel(os?: string): string {
  if (os === undefined) return 'Unknown system';
  return OS_LABELS[os.toLowerCase()] ?? os;
}

/** The first request that comes from the device: by source address, else by host name. */
export function requestOf(
  device: Device,
  requests: readonly PairRequest[],
): PairRequest | undefined {
  const name = device.host_name.toLowerCase();
  return requests.find(
    (r) =>
      device.ips.includes(r.source_ip) ||
      (r.tailscale?.host_name !== undefined && r.tailscale.host_name.toLowerCase() === name),
  );
}

/** What a scope lets the deck do on the station, in plain words. */
export function scopeText(scope: string): string {
  switch (scope) {
    case 'read':
      return 'See its sessions and files';
    case 'drive':
      return 'Start sessions and send prompts';
    case 'approve':
      return 'Answer approvals';
    case 'admin':
      return 'Change its settings and manage it';
    default:
      return scope;
  }
}

/** `m:ss` for a time left in ms (never below 0:00). */
export function countdownText(ms: number): string {
  const total = Math.max(0, Math.ceil(ms / 1000));
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, '0')}`;
}
