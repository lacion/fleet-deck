// R13 P5a task F (design `omarchy-p5a.md` §2.5, §7.2-§7.4): the screenshot part of the Desktop
// panel. `useCapture` keeps the flow of one station (see `flow.ts`) and returns the pieces that
// `DesktopPanel` places: a "Screenshot" button for each monitor, one reason line, and the block
// with the status, the result and the image. A capture needs no confirmation (§2.2).
import { type ReactNode, useEffect, useMemo, useState, useSyncExternalStore } from 'react';
import { rawPath } from '../artifacts/actions';
import type { ArtifactPublished, DesktopInfo, StationView } from '../gen';
import { useSettings } from '../settings/context';
import {
  CaptureFlow,
  DELETED_TEXT,
  type FlowState,
  IDLE,
  isActive,
  SAVED_TEXT,
  UNKNOWN_TEXT,
  WAITING_TEXT,
} from './flow';

export const FEATURE = 'desktop_capture.v1';
/** The board waits this long for the image (§7.1). */
export const IMAGE_LOAD_MS = 65_000;
export const IMAGE_FAILED_TEXT = 'The screenshot could not be loaded. Try again.';

export const REASONS = {
  deck: 'This deck does not support screenshots.',
  station: 'This station does not support screenshots.',
  grim: 'The screenshot tool grim is not installed.',
  off: 'Desktop screenshots are off on this station. Turn them on in Settings.',
  monitor: 'This monitor is not allowed for screenshots.',
  active: 'A screenshot is in progress.',
} as const;

export type Availability = { readonly enabled: boolean; readonly reason: string | null };

/**
 * Whether the screenshot of one monitor can start, and the first reason when it cannot.
 * `deck` is the `/api/features` list: `undefined` while it loads, `null` for an old deck.
 */
export function availability(
  deck: readonly string[] | null | undefined,
  info: DesktopInfo,
  monitor: string,
  active: boolean,
): Availability {
  if (deck === undefined) return { enabled: false, reason: null };
  const no = (reason: string): Availability => ({ enabled: false, reason });
  if (deck === null || !deck.includes(FEATURE)) return no(REASONS.deck);
  if (!info.features.includes(FEATURE)) return no(REASONS.station);
  if (!info.tools.grim) return no(REASONS.grim);
  if (!info.allowed.capture) return no(REASONS.off);
  const allowed = info.allowed.capture_monitors;
  if (allowed.length > 0 && !allowed.includes(monitor)) return no(REASONS.monitor);
  if (active) return no(REASONS.active);
  return { enabled: true, reason: null };
}

type ImageStatus = 'loading' | 'ok' | 'failed';

/** The image and its failure line, for one state (tested without a browser). */
export function CaptureImageView({
  src,
  alt,
  status,
  onLoad,
  onError,
  onRetry,
}: {
  readonly src: string | null;
  readonly alt: string;
  readonly status: ImageStatus;
  readonly onLoad: () => void;
  readonly onError: () => void;
  readonly onRetry: () => void;
}) {
  return (
    <div className="fd-desk-shot">
      {src === null || status === 'failed' ? null : (
        <img className="fd-art-image" src={src} alt={alt} onLoad={onLoad} onError={onError} />
      )}
      {status === 'failed' ? (
        <p role="alert" className="fd-fielderror">
          {IMAGE_FAILED_TEXT}{' '}
          <button type="button" className="fd-ghost fd-sm" onClick={onRetry}>
            Try again
          </button>
        </p>
      ) : null}
    </div>
  );
}

/** The raw URL of the screenshot; `null` when the version lists no file. */
export function shotSrc(published: ArtifactPublished, attempt: number): string | null {
  const { artifact, version } = published;
  const file = version.files[0];
  if (file === undefined) return null;
  const base = rawPath(artifact.station_id, artifact.artifact_id, version.version, file);
  return attempt === 0 ? base : `${base}?r=${attempt}`;
}

/** The inline image with the 65 second load timer; "Try again" reloads the image only. */
function CaptureImage({ published }: { readonly published: ArtifactPublished }) {
  const [attempt, setAttempt] = useState(0);
  const src = shotSrc(published, attempt);
  const [status, setStatus] = useState<ImageStatus>(src === null ? 'failed' : 'loading');
  useEffect(() => {
    if (status !== 'loading') return undefined;
    const timer = setTimeout(() => setStatus('failed'), IMAGE_LOAD_MS);
    return () => clearTimeout(timer);
  }, [status, attempt]);
  return (
    <CaptureImageView
      src={src}
      alt={published.artifact.title}
      status={status}
      onLoad={() => setStatus('ok')}
      onError={() => setStatus('failed')}
      onRetry={() => {
        setAttempt((n) => n + 1);
        setStatus('loading');
      }}
    />
  );
}

/** The status line, the unknown choice, the failure or the result of the last screenshot. */
export function CaptureBlock({
  state,
  onCheck,
  onDismiss,
}: {
  readonly state: FlowState;
  readonly onCheck: () => void;
  readonly onDismiss: () => void;
}): ReactNode {
  switch (state.kind) {
    case 'idle':
      return null;
    case 'taking':
    case 'waiting':
      return <p role="status">{WAITING_TEXT}</p>;
    case 'unknown':
      return (
        <div className="fd-disp-confirm" role="alert">
          <p>{UNKNOWN_TEXT}</p>
          <button type="button" className="fd-ghost" disabled={state.checking} onClick={onCheck}>
            Check result
          </button>
          <button type="button" className="fd-ghost" onClick={onDismiss}>
            Dismiss
          </button>
        </div>
      );
    case 'failed':
      return (
        <p className="fd-fielderror" role="alert">
          {state.text}
        </p>
      );
    case 'deleted':
      return <p role="status">{DELETED_TEXT}</p>;
    case 'committed': {
      const { artifact } = state.published;
      return (
        <div className="fd-desk-result">
          {state.after202 ? <p role="status">{SAVED_TEXT}</p> : null}
          <a
            href={`#/artifact/${encodeURIComponent(artifact.station_id)}/${encodeURIComponent(artifact.artifact_id)}`}
          >
            Open the screenshot
          </a>
          <CaptureImage key={artifact.artifact_id} published={state.published} />
        </div>
      );
    }
  }
}

const NEVER = () => () => {};
const IDLE_SNAPSHOT = () => IDLE;

const storageOrNull = (): Storage | null => {
  try {
    return window.sessionStorage;
  } catch {
    return null;
  }
};

export type CaptureUi = {
  readonly button: (monitorName: string) => ReactNode;
  readonly reason: string | null;
  readonly block: ReactNode;
};

/**
 * The screenshot flow of one station for `DesktopPanel`. Call it once, before any early return.
 * `info` is the desktop that the panel shows; `busy` is true while another action is running.
 */
export function useCapture(
  station: StationView,
  info: DesktopInfo | undefined,
  busy: boolean,
): CaptureUi {
  const env = useSettings();
  const capture = env?.capture;
  const [deck, setDeck] = useState<readonly string[] | null | undefined>(undefined);
  useEffect(() => {
    if (capture === undefined) return undefined;
    let live = true;
    capture.features().then(
      (list) => live && setDeck(list),
      () => live && setDeck(null),
    );
    return () => {
      live = false;
    };
  }, [capture]);
  const flow = useMemo(
    () =>
      capture === undefined
        ? null
        : new CaptureFlow({
            stationName: station.name,
            stationId: station.station_id,
            actions: capture,
            storage: storageOrNull(),
          }),
    [capture, station.name, station.station_id],
  );
  useEffect(() => {
    flow?.attach();
    return () => flow?.detach();
  }, [flow]);
  const state = useSyncExternalStore(
    flow === null ? NEVER : flow.subscribe,
    flow === null ? IDLE_SNAPSHOT : flow.getSnapshot,
    IDLE_SNAPSHOT,
  );
  const active = isActive(state);
  const reasonOf = (monitor: string): Availability =>
    info === undefined
      ? { enabled: false, reason: null }
      : availability(deck, info, monitor, active);
  const reason =
    info === undefined
      ? null
      : (info.monitors.map((m) => reasonOf(m.name).reason).find((r) => r !== null) ?? null);
  return {
    button: (monitorName) => (
      <button
        type="button"
        className="fd-ghost fd-sm"
        disabled={busy || flow === null || !reasonOf(monitorName).enabled}
        onClick={() => void flow?.start(monitorName)}
      >
        Screenshot
      </button>
    ),
    reason,
    block:
      flow === null ? null : (
        <CaptureBlock
          state={state}
          onCheck={() => void flow.check()}
          onDismiss={() => flow.dismiss()}
        />
      ),
  };
}
