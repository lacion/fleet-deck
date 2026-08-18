export interface PageShareInfo {
  remote: boolean;
  proxied: boolean;
  url: string;
}

// The daemon's LAN flag describes its own bind address, but a loopback daemon
// may already be deliberately reachable through a Coder app proxy or tunnel.
// Classify the page origin independently so Share never recommends widening
// the daemon bind when the authenticated proxy is already the public boundary.
export function shareInfoForHref(href: string): PageShareInfo {
  try {
    const u = new URL(href);
    // The board token is a credential. initToken normally scrubs it before
    // React mounts; remove it here too so it can never reappear in a generic
    // proxy/tunnel URL copied from the Share panel.
    u.searchParams.delete('t');
    u.hash = '';
    const host = u.hostname.toLowerCase();
    const loopback =
      host === 'localhost' ||
      host.endsWith('.localhost') ||
      host === '::1' ||
      host === '[::1]' ||
      /^127\./.test(host) ||
      // WHATWG URL canonicalizes ::ffff:127.0.0.1 to ::ffff:7f00:1.
      /^\[::ffff:7f[0-9a-f]{2}:/.test(host);
    // Coder's canonical app route remains a proxy boundary even when Coder
    // itself is being exercised locally at 127.0.0.1. Origin-only detection
    // would recommend exposing fleetd on 0.0.0.0 in precisely that test/setup.
    const coderApp = /^\/@[^/]+\/[^/]+\/apps\/[^/]+(?:\/|$)/.test(u.pathname);
    return { remote: !loopback, proxied: !loopback || coderApp, url: u.toString() };
  } catch {
    return { remote: false, proxied: false, url: '' };
  }
}
