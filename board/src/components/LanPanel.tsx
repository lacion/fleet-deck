import { useMemo, useRef, useState } from 'react';
import { qrPath } from '../qr.ts';
import { copyText } from '../util.ts';
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
  const copy = async () => {
    const ok = await copyText(url);
    setState(ok ? 'ok' : 'manual');
    setTimeout(
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
          <span className="lbl">{enabled ? 'SHARE THIS BOARD' : 'THIS BOARD IS LOCAL ONLY'}</span>
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
