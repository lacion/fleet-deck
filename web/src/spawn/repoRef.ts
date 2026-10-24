// The repository field of a clone start: `owner/name`, `host/owner/name`, a bare name (with the
// default organization) or a URL become one clone URL and a directory name.

/** The deck settings that complete a short repository name (`clone_host`, `clone_org`, `clone_transport`). */
export type CloneDefaults = {
  readonly host: string;
  readonly org: string;
  readonly transport: 'https' | 'ssh';
};

export const DEFAULT_CLONE: CloneDefaults = { host: 'github.com', org: '', transport: 'https' };

export type RepoRef = { readonly url: string; readonly name: string } | { readonly error: string };

const SCHEME_URL = /^[a-z][a-z0-9+.-]*:\/\//i;
const SCP_URL = /^[^@\s/]+@[^:\s/]+:/;
const SEGMENT = /^[A-Za-z0-9._-]+$/;

const withoutGit = (s: string): string => (s.endsWith('.git') ? s.slice(0, -4) : s);

/** The name of the repository in a URL: its last non-empty path segment without `.git`. */
function urlName(url: string): string {
  const scheme = url.match(SCHEME_URL);
  // A scheme URL has its path after the host; an scp URL has it after the first colon.
  let path: string;
  if (scheme !== null) {
    const rest = url.slice(scheme[0].length);
    const slash = rest.indexOf('/');
    path = slash < 0 ? '' : rest.slice(slash);
  } else {
    path = url.slice(url.indexOf(':') + 1);
  }
  const parts = path.split('/').filter((p) => p !== '');
  const last = parts[parts.length - 1];
  return last === undefined ? '' : withoutGit(last);
}

/** Turns what the owner typed into the clone URL and the name of the new directory. */
export function resolveRepo(input: string, d: CloneDefaults): RepoRef {
  const t = input.trim();
  if (t === '') return { error: 'Enter a repository.' };
  if (SCHEME_URL.test(t) || SCP_URL.test(t)) {
    const name = urlName(t);
    return name === '' ? { error: 'The URL has no repository name.' } : { url: t, name };
  }
  const bad = { error: 'Use owner/name, host/owner/name or a URL.' };
  const trimmed = (s: string): string => s.replace(/^\/+|\/+$/g, '');
  const segments = trimmed(withoutGit(trimmed(t))).split('/');
  if (segments.some((s) => !SEGMENT.test(s) || s === '.' || s === '..')) return bad;
  const first = segments[0] ?? '';
  let host: string;
  let path: string[];
  if (first.includes('.') && segments.length >= 3) {
    host = first.toLowerCase();
    path = segments.slice(1);
  } else if (segments.length >= 2) {
    host = d.host;
    path = segments;
  } else if (d.org === '') {
    return { error: 'Set a default organization in Settings, or enter owner/name.' };
  } else {
    host = d.host;
    path = [...d.org.split('/'), first];
  }
  const joined = path.join('/');
  const url = d.transport === 'ssh' ? `git@${host}:${joined}.git` : `https://${host}/${joined}.git`;
  return { url, name: path[path.length - 1] ?? '' };
}

/** The clone defaults in the deck settings view; a missing or odd value gives the built-in one. */
export function cloneDefaultsOf(view: { readonly values: Record<string, unknown> }): CloneDefaults {
  const { clone_host: host, clone_org: org, clone_transport: transport } = view.values;
  return {
    host: typeof host === 'string' && host !== '' ? host : DEFAULT_CLONE.host,
    org: typeof org === 'string' ? org : DEFAULT_CLONE.org,
    transport: transport === 'ssh' ? 'ssh' : DEFAULT_CLONE.transport,
  };
}
