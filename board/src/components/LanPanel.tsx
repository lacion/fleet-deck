import { useEffect, useMemo, useRef, useState } from 'react';
import { qrPath } from '../qr.ts';
import { copyText } from '../util.ts';
import { shareInfoForHref } from '../share.ts';
import { useModal } from '../useModal.ts';
import type { Lan } from '../../../contracts/index.ts';

// v1.7 LAN share. The daemon sends `lan: {enabled, urls}` on /state — the urls
// already carry ?t=<token>, so everything here is display + copy; the board
// never mints a URL or a token of its own.
//
// The QR is encoded in-page (src/qr.js). Deliberately NOT an npm package and
// emphatically NOT an external QR service: the payload IS the credential, and
// handing it to api.qrserver.com would post the keys to your fleet to a
// stranger. It also keeps the board working with no internet at all.
//
// Always black-on-white, in both themes: half the scanners in the world choke
// on an inverted code, and this thing exists to be pointed at with a phone.

interface UrlRowProps {
  url: string;
  primary: boolean;
}

function UrlRow({ url, primary }: UrlRowProps) {
  const [state, setState] = useState<'ok' | 'manual' | null>(null); // null | 'ok' | 'manual'
  const clearTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => clearTimeout(clearTimer.current ?? undefined), []);
  const copy = async () => {
    const ok = await copyText(url);
    setState(ok ? 'ok' : 'manual');
    clearTimeout(clearTimer.current ?? undefined);
    clearTimer.current = setTimeout(
      () => {
        setState(null);
      },
      ok ? 1600 : 4000,
    );
  };
  return (
    <div className={`fd-lanurl${primary ? ' primary' : ''}`}>
      <button
        type="button"
        className="u"
        title="Copy this link"
        onClick={() => {
          void copy();
        }}
      >
        {url}
      </button>
      <span className={`c ${state ?? ''}`}>
        {state === 'ok' ? 'copied' : state === 'manual' ? 'select it and copy' : '⧉ copy'}
      </span>
    </div>
  );
}

interface LanPanelProps {
  lan: Lan | null;
  onClose: () => void;
}

export default function LanPanel({ lan, onClose }: LanPanelProps) {
  // `lan` may be briefly null (H-S1: it arrives on the /state poll, not the WS
  // frame) — every read below is optional-chained, so an absent lan just reads
  // as "local only" until the poll lands.
  const urls = Array.isArray(lan?.urls) ? lan.urls.filter(Boolean) : [];
  const enabled = !!lan?.enabled && urls.length > 0;
  // A loopback-bound daemon can still be deliberately reachable through an
  // authenticated Coder app/reverse proxy. The browser knows that concrete
  // fact from the page origin even on older daemons whose `lan` shape has no
  // proxy metadata. Never tell that user to expose 0.0.0.0 unnecessarily.
  const pageShare = useMemo(() => shareInfoForHref(window.location.href), []);
  const proxied = !enabled && pageShare.proxied;
  const qr = useMemo(() => (enabled ? qrPath(urls[0]) : null), [enabled, urls[0]]);
  const dialogRef = useRef<HTMLDivElement | null>(null);
  useModal(dialogRef); // M-A2

  return (
    <div className="fd-composewrap" onClick={onClose}>
      <div
        className="fd-compose fd-lan"
        role="dialog"
        aria-modal="true"
        aria-label="Share this board"
        ref={dialogRef}
        onClick={(e) => {
          e.stopPropagation();
        }}
      >
        <div style={{ display: 'flex', alignItems: 'center' }}>
          <span className="lbl">
            {enabled
              ? 'SHARE THIS BOARD'
              : proxied
                ? 'THIS BOARD USES A PROXY'
                : 'THIS BOARD IS LOCAL ONLY'}
          </span>
          <span className="fd-spacer" />
          <button type="button" className="fd-x" aria-label="Close" onClick={onClose}>
            ✕
          </button>
        </div>

        {enabled ? (
          <>
            <div className="sub">
              Open the board from your laptop or phone — the link carries the key, so it works
              straight away.
            </div>
            {qr ? (
              <div className="fd-qr">
                <svg
                  viewBox={`0 0 ${qr.side} ${qr.side}`}
                  width="188"
                  height="188"
                  role="img"
                  aria-label={`QR code for ${urls[0] ?? ''}`}
                  shapeRendering="crispEdges"
                >
                  <rect width={qr.side} height={qr.side} fill="#fff" />
                  <path d={qr.d} fill="#000" />
                </svg>
                <div className="cap">point a phone camera at it</div>
              </div>
            ) : (
              // qrPath() only fails past 450 bytes — a URL that long is not a
              // board URL, so say so instead of drawing a code that won't scan
              <div className="fd-lannote">
                that link is too long to put in a QR — use the link below
              </div>
            )}
            <div className="fd-lanurls">
              {urls.map((u, i) => (
                <UrlRow key={u} url={u} primary={i === 0} />
              ))}
            </div>
            <div className="fd-lanwarn">
              ⚠ Anyone on this network who has the link can spawn agents and type into their
              terminals. It is a password — send it like one.
            </div>
          </>
        ) : proxied ? (
          <>
            <div className="sub">
              {pageShare.remote
                ? 'You opened FleetDeck through a workspace proxy or tunnel. Keep the daemon on loopback; share the workspace/app URL using that proxy’s access controls.'
                : 'You opened FleetDeck through a local Coder app proxy. Keep the daemon on loopback; share the externally reachable Coder app URL, not this localhost test URL.'}
            </div>
            {pageShare.remote && pageShare.url && (
              <div className="fd-lanurls">
                <UrlRow url={pageShare.url} primary />
              </div>
            )}
            <div className="fd-lanwarn">
              Do not bind FleetDeck to <span className="mono">0.0.0.0</span> just for this. The
              proxy should remain the public boundary. If this link asks another user for a
              FleetDeck key, the proxy is in token mode; get the key from your administrator.
            </div>
          </>
        ) : (
          <>
            <div className="sub">
              {lan?.enabled
                ? 'The daemon is listening beyond this machine but reported no reachable address — check the URL it printed at startup.'
                : 'The board is bound to loopback, so it only opens on this machine. Nothing else on your network can reach it (and nothing else can drive your agents).'}
            </div>
            <div className="fd-lanhow">
              <div className="h">To open it from your laptop or phone</div>
              <div className="s">Restart the daemon bound to your network:</div>
              {/* the real CLI (package.json bin + bin/fleetdeck.mjs): `fleetdeck serve`
                  in the foreground, or `fleetdeck service restart` under a supervisor.
                  `fleetd up` never existed. */}
              <div className="cmd">$ FLEETDECK_BIND=0.0.0.0 fleetdeck serve</div>
              <div className="s">
                …or <span className="mono">fleetdeck service restart</span> if the daemon runs under
                a supervisor.
              </div>
              <div className="s">
                It prints a link with a key in it (<span className="mono">?t=…</span>) — open that
                link on the other device. Over the network the board <em>requires</em> that key: it
                can spawn agents and type into terminals, so it will not answer a stranger.
              </div>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
