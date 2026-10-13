// R11 (DL-166): the new invite link: the URL with Copy, and the QR code.
import { useEffect, useMemo, useRef, useState } from 'react';
import { Check, Copy, Icon } from '../ui/icons';
import { qrMatrix, qrSvgPath } from '../ui/qr';

const COPIED_MS = 2000;
const QR_QUIET = 4;

/** The QR code: black on white in every theme, so that a phone can scan it. */
export function Qr({ url }: { readonly url: string }) {
  const code = useMemo(() => {
    const m = qrMatrix(url);
    return m === null ? null : { d: qrSvgPath(m, QR_QUIET), side: m.size + QR_QUIET * 2 };
  }, [url]);
  if (code === null) {
    return <p className="fd-note">The link is too long for a QR code; copy it instead.</p>;
  }
  return (
    <svg
      className="fd-devlink-qr"
      role="img"
      aria-label="QR code of the invite link"
      viewBox={`0 0 ${code.side} ${code.side}`}
      shapeRendering="crispEdges"
    >
      <rect width={code.side} height={code.side} fill="#fff" />
      <path d={code.d} fill="#000" />
    </svg>
  );
}

/** The link in a read-only field, with a Copy button. */
export function CopyLink({ url }: { readonly url: string }) {
  const [copied, setCopied] = useState<'idle' | 'done' | 'failed'>('idle');
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(
    () => () => {
      if (timer.current !== undefined) clearTimeout(timer.current);
    },
    [],
  );
  const copy = async () => {
    if (timer.current !== undefined) clearTimeout(timer.current);
    try {
      await navigator.clipboard.writeText(url);
      setCopied('done');
      timer.current = setTimeout(() => setCopied('idle'), COPIED_MS);
    } catch {
      setCopied('failed');
    }
  };
  return (
    <>
      <div className="fd-devlink-url">
        <input
          type="text"
          readOnly
          aria-label="Invite link"
          value={url}
          onFocus={(e) => e.currentTarget.select()}
        />
        <button type="button" onClick={() => void copy()}>
          <Icon of={copied === 'done' ? Check : Copy} size={14} />{' '}
          {copied === 'done' ? 'Copied' : 'Copy link'}
        </button>
      </div>
      {copied === 'failed' ? (
        <p className="fd-note" role="alert">
          Copy failed: select the link and copy it
        </p>
      ) : null}
    </>
  );
}
