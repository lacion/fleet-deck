// R13 P1: the QR code of the wall, as an SVG with a 4-module quiet zone. Black on white in every
// theme, so a phone can scan it.
import { useMemo } from 'react';
import { wallQr } from './qr';

export function QrCode({ url }: { readonly url: string }) {
  const code = useMemo(() => wallQr(url), [url]);
  if (code === null) return null;
  return (
    <svg
      className="fd-wall-qr"
      role="img"
      aria-label={`QR code for ${url}`}
      viewBox={`0 0 ${code.side} ${code.side}`}
      shapeRendering="crispEdges"
    >
      <rect width={code.side} height={code.side} fill="#fff" />
      <path d={code.path} fill="#000" />
    </svg>
  );
}
