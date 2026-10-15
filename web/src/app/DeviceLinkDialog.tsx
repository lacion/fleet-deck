import { useEffect, useMemo, useRef, useState } from 'react';
import type { DeviceLink } from '../gen';
import {
  countdown,
  defaultLinkIndex,
  type DeviceLinkResult,
  fingerprintGroups,
  needsTrustedCertificate,
} from '../runtime/deviceLink';
import { Check, Copy, Icon, X } from '../ui/icons';
import { qrMatrix, qrSvgPath } from '../ui/qr';
import { trapTab, useFocusReturn } from './focus';
import { useOverlay } from './overlay';

type Props = {
  /** Makes one link. The dialog calls it once when it opens, and again for "Try again" and "New link". */
  readonly load: () => Promise<DeviceLinkResult>;
  readonly onClose: () => void;
  /** Unix ms of "now" (tests). */
  readonly now?: () => number;
  /** The first answer, so that a static render shows a state (tests). Given: no first load. */
  readonly initial?: DeviceLinkResult;
};

type State =
  | { readonly kind: 'loading' }
  | { readonly kind: 'done'; readonly result: DeviceLinkResult };

type Copied = 'idle' | 'done' | 'failed';

const COPIED_MS = 2000;
const TICK_MS = 1000;
const QR_QUIET = 4;

/** The QR code: always black on white, also in the dark theme, so that a phone can scan it. */
function Qr({ url }: { readonly url: string }) {
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
      aria-label="QR code of the login link"
      viewBox={`0 0 ${code.side} ${code.side}`}
      shapeRendering="crispEdges"
    >
      <rect width={code.side} height={code.side} fill="#fff" />
      <path d={code.d} fill="#000" />
    </svg>
  );
}

function CopyLink({ url }: { readonly url: string }) {
  const [copied, setCopied] = useState<Copied>('idle');
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
          aria-label="Login link"
          value={url}
          onFocus={(e) => e.currentTarget.select()}
        />
        <button type="button" onClick={() => void copy()}>
          <Icon of={copied === 'done' ? Check : Copy} size={14} />{' '}
          {copied === 'done' ? 'Copied' : 'Copy link'}
        </button>
      </div>
      {copied === 'failed' ? (
        <p className="fd-note fd-devlink-error" role="alert">
          Copy failed: select the link and copy it
        </p>
      ) : null}
    </>
  );
}

function Ready({ link, left }: { readonly link: DeviceLink; readonly left: number }) {
  const [index, setIndex] = useState(() => defaultLinkIndex(link.links));
  const chosen = link.links[index] ?? link.links[0];
  const url = chosen?.url ?? '';
  return (
    <>
      <p className="fd-note">
        Scan the code with the new device, or open the link there. The link works once.
      </p>
      {link.links.length > 1 ? (
        <select
          aria-label="Board address"
          value={index}
          onChange={(e) => setIndex(Number(e.target.value))}
        >
          {link.links.map((l, i) => (
            <option key={l.label} value={i}>
              {l.trust === 'trusted' ? `${l.label} (Trusted)` : l.label}
            </option>
          ))}
        </select>
      ) : null}
      <Qr url={url} />
      <CopyLink key={url} url={url} />
      <p className="fd-note fd-devlink-left" role="timer">
        Ends in {countdown(left)}
      </p>
      {chosen?.trust === 'trusted' ? <p className="fd-note fd-devlink-trusted">Trusted</p> : null}
      {chosen !== undefined && needsTrustedCertificate(chosen) ? (
        <p className="fd-note fd-devlink-needs">
          Needs a trusted certificate: turn on &apos;Trusted certificate from Tailscale&apos; in
          Settings → Deck.
        </p>
      ) : null}
      {chosen?.trust === 'self_signed' ? (
        <p className="fd-note">
          The board uses its own certificate. When the new device warns about it, check that the
          fingerprint starts with {fingerprintGroups(chosen.certificate_sha256)}.
        </p>
      ) : null}
    </>
  );
}

/** "Add a device" (DL-146): a one-time login link as a QR code and as text. */
export function DeviceLinkDialog({ load, onClose, now = Date.now, initial }: Props) {
  const root = useRef<HTMLDivElement>(null);
  const close = useRef<HTMLButtonElement>(null);
  useFocusReturn(root, close);
  useOverlay();
  const [state, setState] = useState<State>(
    initial === undefined ? { kind: 'loading' } : { kind: 'done', result: initial },
  );
  const [, tick] = useState(0);
  const alive = useRef(false);
  const started = useRef(initial !== undefined);
  const run = () => {
    setState({ kind: 'loading' });
    load()
      .catch(
        (error: unknown): DeviceLinkResult => ({
          ok: false,
          text: error instanceof Error ? error.message : 'The request failed.',
        }),
      )
      .then((result) => {
        if (alive.current) setState({ kind: 'done', result });
      });
  };
  // One request for each open: StrictMode runs this effect twice, the ref stops the second run.
  useEffect(() => {
    alive.current = true;
    if (!started.current) {
      started.current = true;
      run();
    }
    return () => {
      alive.current = false;
    };
  }, []);
  const ready = state.kind === 'done' && state.result.ok;
  useEffect(() => {
    if (!ready) return undefined;
    const timer = setInterval(() => tick((n) => n + 1), TICK_MS);
    return () => clearInterval(timer);
  }, [ready]);

  let body: React.ReactNode;
  if (state.kind === 'loading') {
    body = (
      <p className="fd-note" role="status">
        Making a login link…
      </p>
    );
  } else if (!state.result.ok) {
    body = (
      <>
        <p className="fd-note fd-devlink-error" role="alert">
          {state.result.text}
        </p>
        <button type="button" className="fd-ghost" onClick={run}>
          Try again
        </button>
      </>
    );
  } else {
    const left = state.result.link.expires_at - now();
    body =
      left <= 0 ? (
        <>
          <p className="fd-note" role="status">
            The link has ended.
          </p>
          <button type="button" className="fd-primary" onClick={run}>
            New link
          </button>
        </>
      ) : (
        <Ready link={state.result.link} left={left} />
      );
  }

  return (
    // The overlay click is a mouse shortcut; the Close button and Escape serve the keyboard.
    <div
      className="fd-overlay"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        ref={root}
        className="fd-dialog fd-devlink"
        role="dialog"
        aria-modal="true"
        aria-label="Add a device"
        onKeyDown={(e) => {
          if (e.key === 'Escape') {
            e.stopPropagation();
            onClose();
          } else trapTab(e, root.current);
        }}
      >
        <h2>Add a device</h2>
        {body}
        <button ref={close} type="button" className="fd-ghost fd-devlink-close" onClick={onClose}>
          <Icon of={X} size={14} /> Close
        </button>
      </div>
    </div>
  );
}
