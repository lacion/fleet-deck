// R13 P5b (§12): the viewer of a video artifact. A native `<video>` on the version-pinned raw URL
// of the same origin: the browser streams it with Range requests. The bytes are never fetched into
// a Blob or a base64 string; there is no autoplay, no loop and no poster.
import { type RefObject, useRef, useState } from 'react';
import { ArrowDown, Icon } from '../ui/icons';
import { reloadSource } from './video';

export const UNPLAYABLE_TEXT = 'This browser cannot play this video.';

type FrameProps = {
  readonly url: string;
  readonly title: string;
  /** The recording line above the player; `null` for a video that is not a recording. */
  readonly provenance: string | null;
  readonly failed: boolean;
  readonly downloadName: string;
  readonly videoRef?: RefObject<HTMLVideoElement | null>;
  readonly onError?: () => void;
  readonly onRetry?: () => void;
};

/** The player, its provenance line and, after a playback error, the text, Download and Retry. */
export function VideoFrame({
  url,
  title,
  provenance,
  failed,
  downloadName,
  videoRef,
  onError,
  onRetry,
}: FrameProps) {
  return (
    <div className="fd-art-video">
      {provenance === null ? null : <p className="fd-note fd-art-provenance">{provenance}</p>}
      {/* biome-ignore lint/a11y/useMediaCaption: a desktop recording has no audio and no captions */}
      <video
        ref={videoRef}
        className="fd-art-image"
        controls
        preload="metadata"
        src={url}
        aria-label={title}
        hidden={failed}
        onError={onError}
      />
      {failed ? (
        <div role="alert" className="fd-note fd-art-state fd-art-error">
          <p>{UNPLAYABLE_TEXT}</p>
          <div className="fd-art-actions">
            <a className="fd-btn fd-ghost fd-sm fd-art-action" href={url} download={downloadName}>
              <Icon of={ArrowDown} size={14} /> Download
            </a>
            <button type="button" className="fd-ghost fd-sm fd-art-action" onClick={onRetry}>
              Retry
            </button>
          </div>
        </div>
      ) : null}
    </div>
  );
}

/** The video of one version. The caller keys it by the raw URL: a new version starts clean. */
export function VideoView(props: Omit<FrameProps, 'failed' | 'videoRef' | 'onError' | 'onRetry'>) {
  const [failed, setFailed] = useState(false);
  const ref = useRef<HTMLVideoElement | null>(null);
  return (
    <VideoFrame
      {...props}
      failed={failed}
      videoRef={ref}
      onError={() => setFailed(true)}
      onRetry={() => {
        setFailed(false);
        if (ref.current !== null) reloadSource(ref.current, props.url);
      }}
    />
  );
}
