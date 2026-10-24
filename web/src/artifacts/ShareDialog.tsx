import { useEffect, useMemo, useRef, useState } from 'react';
import { trapTab, useFocusReturn } from '../app/focus';
import { useOverlay } from '../app/overlay';
import type { DeckArtifactDetail, ShareLink, ShareView } from '../gen';
import { Check, Copy, Icon, X } from '../ui/icons';
import { qrMatrix, qrSvgPath } from '../ui/qr';
import type { ArtifactActions } from './actions';

/** The expiry choices. The deck allows 1 to 365 days, so "1 year" is the longest. */
export const EXPIRIES: readonly { readonly days: number; readonly label: string }[] = [
  { days: 1, label: '1 day' },
  { days: 7, label: '7 days' },
  { days: 30, label: '30 days' },
  { days: 365, label: '1 year (the longest)' },
];

const COPIED_MS = 2000;
const QR_QUIET = 4;

const message = (e: unknown) => (e instanceof Error ? e.message : 'The request failed.');

/** The UTC date of a unix-ms time, `2026-10-13`. */
const day = (ms: number): string => new Date(ms).toISOString().slice(0, 10);

/** The QR code: black on white in every theme, so that a phone can scan it. */
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
      aria-label="QR code of the share link"
      viewBox={`0 0 ${code.side} ${code.side}`}
      shapeRendering="crispEdges"
    >
      <rect width={code.side} height={code.side} fill="#fff" />
      <path d={code.d} fill="#000" />
    </svg>
  );
}

function CopyLink({ url }: { readonly url: string }) {
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
          aria-label="Share link"
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

/** The new link: the URL with Copy, the QR code and the warning. */
export function ShareResult({ link }: { readonly link: ShareLink }) {
  return (
    <div className="fd-share-result" role="status">
      <CopyLink url={link.url} />
      <Qr url={link.url} />
      <p className="fd-note">Anyone with this link can open it.</p>
      <p className="fd-note">
        {link.version === null ? 'Always the latest version' : `Version ${link.version}`}
        {' · '}
        expires {day(link.expires_at)}
      </p>
    </div>
  );
}

/** The shares of the artifact. The deck shows a URL once, at creation: the list has none. */
export function ShareList({
  shares,
  now,
  busy,
  onRevoke,
}: {
  readonly shares: readonly ShareView[];
  readonly now: number;
  readonly busy: string | null;
  readonly onRevoke: (share: ShareView) => void;
}) {
  if (shares.length === 0) return <p className="fd-note">No share links yet.</p>;
  return (
    <ul className="fd-share-list" aria-label="Share links">
      {shares.map((share) => {
        const expired = share.expires_at <= now;
        const state = share.revoked ? 'revoked' : expired ? 'expired' : 'live';
        return (
          <li key={share.share_id} data-state={state}>
            <span className="fd-share-what">
              {share.version === null ? 'Latest' : `Version ${share.version}`}
            </span>
            <span className="fd-note">
              made {day(share.created_at)} · expires {day(share.expires_at)} · {share.views}{' '}
              {share.views === 1 ? 'view' : 'views'}
            </span>
            {state === 'live' ? (
              <button
                type="button"
                className="fd-ghost fd-sm fd-danger"
                disabled={busy !== null}
                aria-label={`Revoke the share of ${day(share.created_at)}`}
                onClick={() => onRevoke(share)}
              >
                {busy === share.share_id ? 'Revoking…' : 'Revoke'}
              </button>
            ) : (
              <span className="fd-badge" data-tone="muted">
                {state === 'revoked' ? 'Revoked' : 'Expired'}
              </span>
            )}
          </li>
        );
      })}
    </ul>
  );
}

type Props = {
  readonly station: string;
  readonly id: string;
  /** The latest version number: the fixed-version choices are 1 to this. */
  readonly latest: number;
  readonly actions: ArtifactActions;
  readonly initialShares: readonly ShareView[];
  readonly onClose: () => void;
  /** Unix ms of "now" (tests). */
  readonly now?: number;
  /** A link, so that a static render shows the result (tests). */
  readonly initialLink?: ShareLink;
  /** An error text, so that a static render shows it (tests). */
  readonly initialError?: string;
};

/** "Share…" (owner only): make a link, copy it, list the shares and revoke them. */
export function ShareDialog({
  station,
  id,
  latest,
  actions,
  initialShares,
  onClose,
  now: givenNow,
  initialLink,
  initialError,
}: Props) {
  const root = useRef<HTMLDivElement>(null);
  const close = useRef<HTMLButtonElement>(null);
  useFocusReturn(root, close);
  useOverlay();
  const [version, setVersion] = useState('latest');
  const [days, setDays] = useState(7);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(initialError ?? null);
  const [link, setLink] = useState<ShareLink | null>(initialLink ?? null);
  const [shares, setShares] = useState<readonly ShareView[]>(initialShares);
  const [revoking, setRevoking] = useState<string | null>(null);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const now = givenNow ?? Date.now();

  const reload = async () => {
    try {
      const detail: DeckArtifactDetail = await actions.detail(station, id);
      if (alive.current) setShares(detail.shares);
    } catch {
      // The list stays as it was; the new link is already shown.
    }
  };
  const create = async () => {
    setBusy(true);
    setError(null);
    setLink(null);
    try {
      const made = await actions.share(station, id, {
        version: version === 'latest' ? null : Number(version),
        expires_in_days: days,
      });
      if (!alive.current) return;
      setLink(made);
      await reload();
    } catch (e) {
      if (alive.current) setError(message(e));
    } finally {
      if (alive.current) setBusy(false);
    }
  };
  const revoke = async (share: ShareView) => {
    setRevoking(share.share_id);
    setError(null);
    try {
      await actions.revoke(station, id, share.share_id);
      await reload();
    } catch (e) {
      if (alive.current) setError(message(e));
    } finally {
      if (alive.current) setRevoking(null);
    }
  };

  const versions = Array.from({ length: Math.max(latest, 0) }, (_, n) => n + 1).reverse();
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
        className="fd-dialog fd-share"
        role="dialog"
        aria-modal="true"
        aria-label="Share the artifact"
        onKeyDown={(e) => {
          if (e.key === 'Escape') {
            e.stopPropagation();
            onClose();
          } else trapTab(e, root.current);
        }}
      >
        <h2>Share the artifact</h2>
        <form
          className="fd-share-form"
          aria-label="Make a share link"
          onSubmit={(e) => {
            e.preventDefault();
            void create();
          }}
        >
          <label>
            Version
            <select value={version} onChange={(e) => setVersion(e.target.value)}>
              <option value="latest">Latest (follows new versions)</option>
              {versions.map((n) => (
                <option key={n} value={String(n)}>
                  Version {n}
                </option>
              ))}
            </select>
          </label>
          <label>
            Expires after
            <select value={days} onChange={(e) => setDays(Number(e.target.value))}>
              {EXPIRIES.map((x) => (
                <option key={x.days} value={x.days}>
                  {x.label}
                </option>
              ))}
            </select>
          </label>
          <button type="submit" className="fd-primary" disabled={busy}>
            {busy ? 'Making the link…' : 'Make a link'}
          </button>
        </form>
        {error === null ? null : (
          <p className="fd-note fd-share-error" role="alert">
            {error}
          </p>
        )}
        {link === null ? null : <ShareResult link={link} />}
        <h3 className="fd-label">Share links</h3>
        <ShareList
          shares={shares}
          now={now}
          busy={revoking}
          onRevoke={(share) => void revoke(share)}
        />
        <button ref={close} type="button" className="fd-ghost fd-devlink-close" onClick={onClose}>
          <Icon of={X} size={14} /> Close
        </button>
      </div>
    </div>
  );
}
