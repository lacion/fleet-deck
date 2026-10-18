import { useState } from 'react';
import { Markdown } from '../chat/Markdown';
import type { DeckArtifactDetail } from '../gen';
import { ArrowDown, Icon, Link as LinkIcon } from '../ui/icons';
import { type ArtifactActions, rawPath } from './actions';
import {
  CONFIRM_TEXT,
  DELETE_LABEL,
  type DeletionView,
  stateText,
  useDeletion,
} from './DeleteArtifact';
import { fileName, KIND_LABEL } from './kinds';
import { SendToWall } from '../wall/SendToWall';
import { ShareDialog } from './ShareDialog';
import { mainFile, type ViewerData, type ViewerReady, useViewer } from './viewer';

/** The permissions of the frame. No `allow-same-origin`: the page gets an opaque origin (DL-165). */
export const FRAME_SANDBOX = 'allow-scripts allow-popups allow-forms allow-modals';

/** The sandboxed frame of an HTML, SVG or PDF artifact. */
export function ArtifactFrame({ url, title }: { readonly url: string; readonly title: string }) {
  return (
    <iframe
      className="fd-art-frame"
      title={title}
      src={url}
      sandbox={FRAME_SANDBOX}
      referrerPolicy="no-referrer"
    />
  );
}

function Loading({ what }: { readonly what: string }) {
  return (
    <p role="status" className="fd-note fd-art-state">
      {what}
    </p>
  );
}

function Failed({ text }: { readonly text: string }) {
  return (
    <p role="alert" className="fd-note fd-art-state fd-art-error">
      {text}
    </p>
  );
}

/** What the shown version looks like: a frame, an image, rendered Markdown or a note. */
function Body({ ready }: { readonly ready: ViewerReady }) {
  const { detail, link, text } = ready;
  const { title, kind } = detail.artifact;
  if (kind === 'markdown') {
    if (text === null || text.kind === 'loading') return <Loading what="Loading the document…" />;
    if (text.kind === 'error') return <Failed text={text.text} />;
    return (
      <div className="fd-art-md">
        <Markdown text={text.value} />
      </div>
    );
  }
  if (kind === 'other') {
    return (
      <p className="fd-note fd-art-state">
        This file type has no preview. Use Download to open it.
      </p>
    );
  }
  if (link.kind === 'loading') return <Loading what="Opening the artifact…" />;
  if (link.kind === 'error') return <Failed text={link.text} />;
  if (kind === 'image') {
    return <img className="fd-art-image" src={link.value} alt={title} />;
  }
  return <ArtifactFrame url={link.value} title={title} />;
}

type ViewProps = {
  readonly data: ViewerData;
  readonly variant: 'panel' | 'page';
  readonly canShare: boolean;
  readonly onVersion: (version: number) => void;
  /** The "back" control: a button in the panel, a link in the page. */
  readonly onBack?: (() => void) | undefined;
  readonly onShare: () => void;
  /** The owner's "Delete artifact"; absent: no control (R13 P5a). */
  readonly deletion?: DeletionView | undefined;
};

/** The confirmation, the error or the pending text of a deletion. */
function DeletionNotes({ deletion }: { readonly deletion: DeletionView }) {
  const { phase } = deletion;
  if (phase.kind === 'confirm') {
    return (
      <div className="fd-disp-confirm" role="alertdialog" aria-label="Confirm">
        <p>{CONFIRM_TEXT}</p>
        <button type="button" className="fd-primary" onClick={deletion.onConfirm}>
          Confirm
        </button>
        <button type="button" className="fd-ghost" onClick={deletion.onCancel}>
          Cancel
        </button>
      </div>
    );
  }
  if (phase.kind === 'error') return <Failed text={phase.text} />;
  return null;
}

/** The viewer without data calls: the toolbar and the body of one state. */
export function ArtifactViewerView({
  data,
  variant,
  canShare,
  onVersion,
  onBack,
  onShare,
  deletion,
}: ViewProps) {
  const back =
    variant === 'page' ? (
      <a className="fd-ghost fd-sm fd-art-back" href="#/artifacts">
        All artifacts
      </a>
    ) : (
      <button type="button" className="fd-ghost fd-sm fd-art-back" onClick={onBack}>
        Back to the list
      </button>
    );
  if (data.kind !== 'ok') {
    return (
      <section className="fd-art fd-art-viewer" aria-label="Artifact" data-variant={variant}>
        <div className="fd-art-bar">{back}</div>
        {data.kind === 'loading' ? (
          <Loading what="Loading the artifact…" />
        ) : (
          <Failed text={data.text} />
        )}
      </section>
    );
  }
  const ready = data.value;
  const { artifact, versions } = ready.detail;
  const main = mainFile(ready.detail, ready.version);
  const viewUrl = ready.link.kind === 'ok' ? ready.link.value : null;
  return (
    <section className="fd-art fd-art-viewer" aria-label="Artifact" data-variant={variant}>
      <div className="fd-art-bar">
        {back}
        <h2 className="fd-art-name">{artifact.title}</h2>
        <span className="fd-badge" data-kind={artifact.kind}>
          {KIND_LABEL[artifact.kind]}
        </span>
        <label className="fd-art-version">
          <span className="fd-vh">Version</span>
          <select
            aria-label="Version"
            value={ready.version}
            onChange={(e) => onVersion(Number(e.target.value))}
          >
            {[...versions].reverse().map((v) => (
              <option key={v.version} value={v.version}>
                {v.version === artifact.latest_version ? `v${v.version} (latest)` : `v${v.version}`}
              </option>
            ))}
          </select>
        </label>
        {viewUrl === null ? null : (
          <a
            className="fd-ghost fd-sm fd-art-action"
            href={viewUrl}
            target="_blank"
            rel="noopener noreferrer"
          >
            <Icon of={LinkIcon} size={14} /> Open in a new tab
          </a>
        )}
        {main === undefined ? null : (
          <a
            className="fd-ghost fd-sm fd-art-action"
            href={rawPath(artifact.station_id, artifact.artifact_id, ready.version, main)}
            download={fileName(main)}
          >
            <Icon of={ArrowDown} size={14} /> Download
          </a>
        )}
        <SendToWall
          className="fd-ghost fd-sm fd-art-action"
          target={{
            kind: 'artifact',
            station_id: artifact.station_id,
            artifact_id: artifact.artifact_id,
            version: ready.version,
          }}
        />
        {canShare ? (
          <button type="button" className="fd-ghost fd-sm fd-art-action" onClick={onShare}>
            Share…
          </button>
        ) : null}
        {deletion === undefined ||
        (deletion.phase.kind !== 'idle' && deletion.phase.kind !== 'error') ? null : (
          <button type="button" className="fd-ghost fd-sm fd-art-action" onClick={deletion.onBegin}>
            {DELETE_LABEL}
          </button>
        )}
      </div>
      {deletion === undefined ? null : <DeletionNotes deletion={deletion} />}
      <div className="fd-art-body" data-kind={artifact.kind}>
        {deletion?.phase.kind === 'hidden' ? (
          <p role="status" className="fd-note fd-art-state">
            {stateText(deletion.phase.state)}
          </p>
        ) : (
          <Body ready={ready} />
        )}
      </div>
    </section>
  );
}

type Props = {
  readonly actions: ArtifactActions;
  readonly station: string;
  readonly id: string;
  /** `null`: the latest version. */
  readonly version: number | null;
  readonly variant: 'panel' | 'page';
  readonly onVersion: (version: number) => void;
  readonly onBack?: () => void;
};

/** The viewer of one artifact: a full-height panel in the session view or a full page. */
export function ArtifactViewer({
  actions,
  station,
  id,
  version,
  variant,
  onVersion,
  onBack,
}: Props) {
  const [reload, setReload] = useState(0);
  const [sharing, setSharing] = useState(false);
  const data = useViewer(actions, station, id, version, reload);
  const detail: DeckArtifactDetail | null = data.kind === 'ok' ? data.value.detail : null;
  // The owner's delete: a deletion that finishes at once leaves the viewer for the list.
  const { view: deletion, node: elevationNode } = useDeletion(station, id, actions.canShare, () => {
    if (variant === 'panel') onBack?.();
    else window.location.hash = '#/artifacts';
  });
  return (
    <>
      <ArtifactViewerView
        data={data}
        variant={variant}
        canShare={actions.canShare}
        onVersion={onVersion}
        onBack={onBack}
        onShare={() => setSharing(true)}
        deletion={deletion}
      />
      {elevationNode}
      {sharing && detail !== null ? (
        <ShareDialog
          station={station}
          id={id}
          latest={detail.artifact.latest_version}
          actions={actions}
          initialShares={detail.shares}
          onClose={() => {
            setSharing(false);
            setReload((n) => n + 1);
          }}
        />
      ) : null}
    </>
  );
}

/** The full page `#/artifact/<station>/<id>[/<version>]`. */
export function ArtifactPage({
  actions,
  station,
  id,
  version,
  onVersion,
}: {
  readonly actions: ArtifactActions | undefined;
  readonly station: string;
  readonly id: string;
  readonly version: number | null;
  readonly onVersion: (version: number) => void;
}) {
  if (actions === undefined) {
    return (
      <section className="fd-screen" aria-label="Artifact">
        <p className="fd-note">Artifacts need a live board.</p>
        <a href="#/artifacts">All artifacts</a>
      </section>
    );
  }
  return (
    <ArtifactViewer
      key={`${station}/${id}`}
      actions={actions}
      station={station}
      id={id}
      version={version}
      variant="page"
      onVersion={onVersion}
    />
  );
}
