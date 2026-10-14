// DL-178: the pure rules of the tailnet card and the pairing tab.
import type { TailnetView, WindowView } from './api';

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
