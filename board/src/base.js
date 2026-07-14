// base.js — where the board thinks it is.
//
// PREFIX CONTRACT. The board is served by the daemon at the root of its own
// port, so for a decade of local use "/state" was simply correct. Standalone
// broke that: a path-based reverse proxy (Coder serves apps at
// /@user/workspace.agent/apps/<slug>/) STRIPS its prefix before forwarding, and
// tells the app nothing about it — no X-Forwarded-Prefix, no base-path header.
// So the daemon still sees "/state" and must keep seeing "/state", while the
// BROWSER must ask for "/@user/ws.main/apps/fleetdeck/state". Only the browser
// can know the difference, and only from the one thing that carries the truth:
// the URL its own code was loaded from.
//
// Hence import.meta.url rather than document.baseURI. baseURI is the document
// URL, which is a lie whenever the user lands on the prefix without a trailing
// slash (".../apps/fleetdeck" would resolve "state" to ".../apps/state").
// Coder happens to force a trailing-slash redirect, but relying on that would
// make us wrong on every proxy that does not.
//
// Under a subdomain app, or plain localhost, the prefix is "/" and every URL
// below is byte-identical to what it was before this file existed.

function computeBase() {
  // Vite dev serves modules from /src/*, not /assets/* — and always at the root.
  if (import.meta.env?.DEV) return new URL('/', window.location.origin);
  try {
    const here = new URL(import.meta.url);
    // The built entry chunk always lives at <prefix>/assets/<name>.js.
    const prefix = here.pathname.replace(/\/assets\/[^/]*$/, '/');
    // No match ⇒ an asset layout we do not recognise; "/" is what we did before.
    if (prefix === here.pathname) return new URL('/', window.location.origin);
    return new URL(prefix, here.origin);
  } catch {
    return new URL('/', window.location.origin);
  }
}

const BASE = computeBase();

/** Absolute http(s) URL for a daemon path written root-relative ("/state"). */
export function apiUrl(path) {
  return new URL(String(path).replace(/^\/+/, ''), BASE).toString();
}

/** Absolute ws(s) URL for a daemon path, following the page's own scheme. */
export function wsBase(path) {
  const url = new URL(String(path).replace(/^\/+/, ''), BASE);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  return url;
}
