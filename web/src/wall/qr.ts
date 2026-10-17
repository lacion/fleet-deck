// R13 P1: the QR code of a needs-you item. The encoder is `ui/qr.ts` (byte mode, level M);
// this file adds the answer URL and the geometry that the wall draws.
import { formatHash } from '../app/routes';
import { type QrMatrix, qrMatrix, qrSvgPath } from '../ui/qr';

/** The quiet zone, in modules (ISO/IEC 18004 asks for 4). */
export const QR_QUIET = 4;

/**
 * `<origin>/#/s/<station>/<session>?request=<request id>`. The URL holds no token: the phone needs
 * its own owner login.
 */
export function answerUrl(
  origin: string,
  station: string,
  session: string,
  requestId: string,
): string {
  const base = formatHash({ name: 'session', station, session, tab: 'chat' });
  return `${origin}/${base}?request=${encodeURIComponent(requestId)}`;
}

export type WallQr = {
  readonly matrix: QrMatrix;
  /** The SVG path of the dark modules, shifted by the quiet zone. */
  readonly path: string;
  /** The side of the symbol with its quiet zone, in modules. */
  readonly side: number;
};

/** The symbol of `url`, or `null` when the URL is longer than the encoder takes. */
export function wallQr(url: string): WallQr | null {
  const matrix = qrMatrix(url);
  if (matrix === null) return null;
  return { matrix, path: qrSvgPath(matrix, QR_QUIET), side: matrix.size + QR_QUIET * 2 };
}
