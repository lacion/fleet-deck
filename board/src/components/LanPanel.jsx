import React, { useMemo, useState } from 'react';
import { qrPath } from '../qr.js';
import { copyText } from '../util.js';

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

function UrlRow({ url, primary }) {
  const [state, setState] = useState(null); // null | 'ok' | 'manual'
  const copy = async () => {
    const ok = await copyText(url);
    setState(ok ? 'ok' : 'manual');
    setTimeout(() => setState(null), ok ? 1600 : 4000);
  };
  return (
    <div className={`fd-lanurl${primary ? ' primary' : ''}`}>
      <button
        type="button"
        className="u"
        title="Copy this link"
        onClick={copy}
      >
        {url}
      </button>
      <span className={`c ${state || ''}`}>
        {state === 'ok' ? 'copied' : state === 'manual' ? 'select it and copy' : '⧉ copy'}
      </span>
    </div>
  );
}

export default function LanPanel({ lan, onClose }) {
  const urls = Array.isArray(lan?.urls) ? lan.urls.filter(Boolean) : [];
  const enabled = !!lan?.enabled && urls.length > 0;
  const qr = useMemo(() => (enabled ? qrPath(urls[0]) : null), [enabled, urls[0]]);

  return (
    <div className="fd-composewrap" onClick={onClose}>
      <div className="fd-compose fd-lan" role="dialog" aria-label="Share this board" onClick={(e) => e.stopPropagation()}>
        <div style={{ display: 'flex', alignItems: 'center' }}>
          <span className="lbl">{enabled ? 'SHARE THIS BOARD' : 'THIS BOARD IS LOCAL ONLY'}</span>
          <span className="fd-spacer" />
          <button type="button" className="fd-x" aria-label="Close" onClick={onClose}>✕</button>
        </div>

        {enabled ? (
          <>
            <div className="sub">
              Open the board from your laptop or phone — the link carries the key, so it works straight away.
            </div>
            {qr ? (
              <div className="fd-qr">
                <svg
                  viewBox={`0 0 ${qr.side} ${qr.side}`}
                  width="188"
                  height="188"
                  role="img"
                  aria-label={`QR code for ${urls[0]}`}
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
              <div className="fd-lannote">that link is too long to put in a QR — use the link below</div>
            )}
            <div className="fd-lanurls">
              {urls.map((u, i) => <UrlRow key={u} url={u} primary={i === 0} />)}
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
              <div className="cmd">$ FLEETDECK_BIND=0.0.0.0 fleetd up</div>
              <div className="s">
                It prints a link with a key in it (<span className="mono">?t=…</span>) — open that link on the
                other device. Over the network the board <em>requires</em> that key: it can spawn agents and
                type into terminals, so it will not answer a stranger.
              </div>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
