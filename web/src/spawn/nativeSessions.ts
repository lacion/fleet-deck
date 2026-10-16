// B7: browse the sessions of a station (design native-sessions.md §2.5, §2.6). The fetch helpers
// of the two deck routes and the pure texts and groups of the board dialog.
import type {
  NativeHome,
  NativePreview,
  NativePreviewQuery,
  NativeSession,
  NativeSessionList,
  ProviderKind,
  StationProvider,
  TranscriptPin,
} from '../gen';
import { relativeAge } from '../app/sessions';
import { ApiError } from '../runtime/api';

type Get = (path: string) => Promise<unknown>;
type Post = (path: string, body: unknown) => Promise<unknown>;

/** The two reads of the dialog (owner login only, design §2.5). */
export type NativeActions = {
  list(stationId: string): Promise<NativeSessionList>;
  preview(stationId: string, query: NativePreviewQuery): Promise<NativePreview>;
};

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

/** True when `v` has the shape of a `NativeSessionList` (the deck passes the station's answer on). */
export function isNativeList(v: unknown): v is NativeSessionList {
  return (
    record(v) &&
    Array.isArray(v['homes']) &&
    Array.isArray(v['sessions']) &&
    typeof v['truncated'] === 'boolean' &&
    typeof v['timed_out'] === 'boolean'
  );
}

/** True when `v` has the shape of a `NativePreview`. */
export function isNativePreview(v: unknown): v is NativePreview {
  return (
    record(v) &&
    Array.isArray(v['items']) &&
    record(v['state']) &&
    typeof v['state']['kind'] === 'string' &&
    typeof v['cwd'] === 'string' &&
    typeof v['earlier_omitted'] === 'boolean'
  );
}

export function nativeActions(get: Get, post: Post): NativeActions {
  const base = (stationId: string) =>
    `/api/stations/${encodeURIComponent(stationId)}/native-sessions`;
  return {
    list: async (stationId) => {
      const v = await get(base(stationId));
      if (!isNativeList(v)) throw new Error('The deck sent a list that the board cannot read.');
      return v;
    },
    preview: async (stationId, query) => {
      const v = await post(`${base(stationId)}/preview`, query);
      if (!isNativePreview(v)) {
        throw new Error('The deck sent a preview that the board cannot read.');
      }
      return v;
    },
  };
}

/** The line of a failed read; `retry` shows a "Try again" button (none for a 409). */
export type NativeError = { readonly text: string; readonly retry: boolean };

/** The deck's fixed text of a failure; a 409 (an older station) has no retry. */
export function nativeError(e: unknown): NativeError {
  if (e instanceof ApiError) return { text: e.message, retry: e.status !== 409 };
  return { text: e instanceof Error ? e.message : 'The request failed.', retry: true };
}

/** The muted text of an import state; `null` for `ok`. `link` is the session to open. */
export function importStateLine(
  state: NativeSession['import'],
): { readonly text: string; readonly link: string | null } | null {
  switch (state.state) {
    case 'ok':
      return null;
    case 'outside_roots':
      return { text: "The folder is outside this station's allowed folders.", link: null };
    case 'in_use':
      return { text: 'A Fleet Deck session uses it.', link: state.session_id };
    case 'held':
      return { text: 'A Fleet Deck session holds it.', link: state.session_id };
    case 'execution_unknown':
      return { text: 'It may still run elsewhere. Close it there first.', link: null };
    case 'cwd_mismatch':
      return {
        text: 'Its folder path changed (a link?). Open it from its real folder.',
        link: null,
      };
    case 'too_large':
      return { text: 'The transcript is too large to import.', link: null };
    case 'subagent_thread':
      return { text: 'A subagent thread. Continue its parent.', link: null };
    case 'path_not_trusted':
      return { text: 'Another user can change this folder.', link: null };
    case 'not_trusted':
      return { text: 'Fleet Deck cannot trust this transcript.', link: null };
  }
}

/** The note of an importable entry whose folder Fleet Deck cannot verify; `null` otherwise. */
export function unverifiedNote(s: NativeSession): string | null {
  return s.import.state === 'ok' && !s.verified ? 'Fleet Deck cannot verify this folder.' : null;
}

/** The notes under the list: a cut list, and a list that stopped early. */
export function listNotes(list: NativeSessionList): string[] {
  const notes: string[] = [];
  if (list.truncated) notes.push(`Showing the newest ${list.sessions.length} sessions.`);
  if (list.timed_out) notes.push('The list stopped early; it may be incomplete.');
  return notes;
}

/** The words for the reason that a provider home is not listed. */
export function homeReasonText(reason: string): string {
  switch (reason) {
    case 'home_not_absolute':
      return 'the home folder is not an absolute path.';
    case 'not_readable':
      return 'the home folder cannot be read.';
    case 'home_not_trusted':
      return 'another user can change the home folder.';
    default:
      return reason;
  }
}

/** The one muted line of a home that is not listed; `null` for a home that is ok. */
export function homeLine(home: NativeHome): string | null {
  if (home.state.state !== 'unavailable') return null;
  return `${home.instances.join(', ')}: ${homeReasonText(home.state.reason)}`;
}

/** "5m ago", or "now". */
export function ageText(modifiedAt: number, now: number): string {
  const age = relativeAge(modifiedAt, now);
  return age === 'now' ? age : `${age} ago`;
}

/** A size in decimal units: `512 B`, `12.3 KB`, `4.5 MB`, `1.2 GB`. */
export function sizeText(bytes: number): string {
  if (bytes < 1000) return `${bytes} B`;
  if (bytes < 1e6) return `${(bytes / 1e3).toFixed(1)} KB`;
  if (bytes < 1e9) return `${(bytes / 1e6).toFixed(1)} MB`;
  return `${(bytes / 1e9).toFixed(1)} GB`;
}

/** The one line that names an entry: its title, else its first prompt, else its id. */
export function entryTitle(s: NativeSession): string {
  const title = s.title?.trim() ?? '';
  if (title !== '') return title;
  const prompt = s.first_prompt?.trim() ?? '';
  return prompt !== '' ? prompt.replace(/\s+/g, ' ') : s.native_id;
}

/** The first instance of the entry that the station reports, else its first instance. */
export function entryInstance(s: NativeSession, providers: readonly StationProvider[]): string {
  const known = new Set(providers.map((p) => p.instance));
  return s.instances.find((i) => known.has(i)) ?? s.instances[0] ?? '';
}

/** One row of the list; a Codex child thread has depth 1. */
export type NativeRow = { readonly session: NativeSession; readonly depth: 0 | 1 };

/** The entries of one provider instance, and the homes of it that are not listed. */
export type NativeGroup = {
  readonly instance: string;
  readonly heading: string;
  readonly rows: readonly NativeRow[];
  readonly unavailable: readonly string[];
};

const capital = (kind: ProviderKind): string => kind.charAt(0).toUpperCase() + kind.slice(1);

/**
 * The list grouped by provider instance: the groups follow the station's providers, then any
 * other instance in the order of the list. In a group the order is the station's (newest
 * first); a child thread follows its parent when the parent is in the group, else it stays in
 * place.
 */
export function groupNative(
  list: NativeSessionList,
  providers: readonly StationProvider[],
): NativeGroup[] {
  const kinds = new Map<string, ProviderKind>(providers.map((p) => [p.instance, p.provider]));
  const order = providers.map((p) => p.instance);
  const sessions = new Map<string, NativeSession[]>();
  const lines = new Map<string, string[]>();
  const note = (instance: string, kind: ProviderKind) => {
    if (!kinds.has(instance)) kinds.set(instance, kind);
    if (!order.includes(instance)) order.push(instance);
  };
  for (const s of list.sessions) {
    const instance = entryInstance(s, providers);
    note(instance, s.provider);
    sessions.set(instance, [...(sessions.get(instance) ?? []), s]);
  }
  for (const home of list.homes) {
    const text = homeLine(home);
    if (text === null) continue;
    const instance = home.instances.find((i) => kinds.has(i)) ?? home.instances[0] ?? '';
    note(instance, home.provider);
    lines.set(instance, [...(lines.get(instance) ?? []), text]);
  }
  return order
    .filter((i) => sessions.has(i) || lines.has(i))
    .map((instance) => {
      const mine = sessions.get(instance) ?? [];
      const listed = new Set(mine.map((s) => s.native_id));
      const rows: NativeRow[] = [];
      for (const s of mine) {
        const parent = s.parent_native_id;
        if (parent !== null && listed.has(parent)) continue;
        rows.push({ session: s, depth: parent === null ? 0 : 1 });
        for (const child of mine) {
          if (child.parent_native_id === s.native_id) rows.push({ session: child, depth: 1 });
        }
      }
      const kind = kinds.get(instance) ?? 'unknown';
      return {
        instance,
        heading: `${instance} · ${capital(kind)}`,
        rows,
        unavailable: lines.get(instance) ?? [],
      };
    });
}

/** What "Continue it" puts into the New session form. */
export type NativeFill = {
  readonly instance: string;
  readonly cwd: string;
  readonly importId: string;
  /** Kept in the form for the import pin task; the import command does not send it yet. */
  readonly pin: TranscriptPin;
};

/** The form fields of an entry; `null` unless it can be imported into a provider of the station. */
export function continueFill(
  s: NativeSession,
  providers: readonly StationProvider[],
): NativeFill | null {
  const instance = entryInstance(s, providers);
  if (s.import.state !== 'ok' || s.cwd === null) return null;
  if (!providers.some((p) => p.instance === instance)) return null;
  return { instance, cwd: s.cwd, importId: s.native_id, pin: s.pin };
}

/** The preview request of an entry. */
export function previewQuery(s: NativeSession, instance: string): NativePreviewQuery {
  return { provider_instance: instance, native_id: s.native_id, pin: s.pin };
}
