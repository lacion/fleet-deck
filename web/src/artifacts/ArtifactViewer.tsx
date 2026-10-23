import { useState } from 'react';
import { usePhoneTitle } from '../app/Layout';
import { relativeAge, sessionTitle } from '../app/sessions';
import { Markdown } from '../chat/Markdown';
import type { DeckArtifactDetail, DeckSession } from '../gen';
import { useSettings } from '../settings/context';
import {
  ArrowDown,
  ChevronLeft,
  Ellipsis,
  Icon,
  Link as LinkIcon,
  Share2,
  Trash2,
} from '../ui/icons';
import { Loading } from '../ui/Loading';
import { Menu } from '../ui/Menu';
import { ErrorNotice, Page } from '../ui/page';
import { useNow } from '../ui/useNow';
import { type ArtifactActions, rawPath } from './actions';
import {
  CONFIRM_TEXT,
  DELETE_LABEL,
  type DeletionView,
  stateText,
  useDeletion,
} from './DeleteArtifact';
import { fileName, KIND_LABEL } from './kinds';
import { SendToWall, type WallTarget } from '../wall/SendToWall';
import { ShareDialog } from './ShareDialog';
import { provenanceLine } from './video';
import { VideoView } from './VideoView';
import { mainFile, type ViewerData, type ViewerReady, useViewer, versionRow } from './viewer';

/** The permissions of the frame. No `allow-same-origin`: the page gets an opaque origin (DL-165). */
export const FRAME_SANDBOX = 'allow-scripts allow-popups allow-forms allow-modals';

/** The separator of the summary parts: a no-break space, a dot and a space. */
const DOT = '\u00a0· ';

/** The way back from the artifact page to the list (§4.3). */
const ALL_ARTIFACTS = { href: '#/artifacts', label: 'All artifacts' } as const;

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

/**
 * What the shown version looks like: a frame, an image, rendered Markdown or a note. A load that
 * failed is the `ErrorNotice` of a panel (§4.7), with Try again when the viewer can load again.
 */
function Body({
  ready,
  onRetry,
}: {
  readonly ready: ViewerReady;
  readonly onRetry: (() => void) | undefined;
}) {
  const { detail, link, text } = ready;
  const { title, kind } = detail.artifact;
  if (kind === 'markdown') {
    if (text === null || text.kind === 'loading') return <Loading text="Loading the document…" />;
    if (text.kind === 'error') return <ErrorNotice text={text.text} onRetry={onRetry} />;
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
  if (kind === 'video') {
    // The video needs no view link: the player reads the version-pinned raw route (R13 P5b §12).
    const main = mainFile(detail, ready.version);
    if (main === undefined) {
      return (
        <ErrorNotice
          text="This version has no files yet. The station has not sent them to the deck."
          onRetry={onRetry}
        />
      );
    }
    const { station_id: station, artifact_id: id, station_name: stationName } = detail.artifact;
    const url = rawPath(station, id, ready.version, main);
    return (
      <VideoView
        key={url}
        url={url}
        title={title}
        provenance={provenanceLine(stationName, versionRow(detail, ready.version)?.capture)}
        downloadName={fileName(main)}
      />
    );
  }
  if (link.kind === 'loading') return <Loading text="Opening the artifact…" />;
  if (link.kind === 'error') return <ErrorNotice text={link.text} onRetry={onRetry} />;
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
  /** Loads the artifact again (Try again of a load error); absent: no Try again. */
  readonly onRetry?: (() => void) | undefined;
  /** The owner's "Delete artifact"; absent: no control (R13 P5a). */
  readonly deletion?: DeletionView | undefined;
  /** The sessions of the board: the page names the session that published the artifact. */
  readonly sessions?: readonly DeckSession[] | undefined;
  /** Unix ms of "now" (tests). */
  readonly now?: number | undefined;
};

/** The confirmation, the error or the pending text of a deletion. */
function DeletionNotes({ deletion }: { readonly deletion: DeletionView }) {
  const { phase } = deletion;
  if (phase.kind === 'confirm') {
    return (
      <div className="fd-disp-confirm fd-art-confirm" role="alertdialog" aria-label="Confirm">
        <p>{CONFIRM_TEXT}</p>
        <button type="button" className="fd-danger fd-sm" onClick={deletion.onConfirm}>
          Confirm
        </button>
        <button type="button" className="fd-ghost fd-sm" onClick={deletion.onCancel}>
          Cancel
        </button>
      </div>
    );
  }
  if (phase.kind === 'error') return <ErrorNotice text={phase.text} />;
  return null;
}

/** Starts the download of a file, the same as a click on an `<a download>` link. */
function download(url: string, name: string) {
  const link = document.createElement('a');
  link.href = url;
  link.download = name;
  link.click();
}

/**
 * The trailing "More actions" menu of the viewer bar: Send to wall… and Delete artifact (last, in
 * the danger style). A narrow viewer (a phone, the session panel) moves Download and Share… into
 * it too (`.fd-art-narrow`, artifacts.css); a wide one shows them in the bar. Without a row for the
 * wide bar, the menu shows only on a narrow viewer (`data-wide="none"`).
 */
function MoreActions({
  wall,
  target,
  download: file,
  onShare,
  onDelete,
}: {
  /** The owner's settings are there, so Send to wall… renders. */
  readonly wall: boolean;
  readonly target: WallTarget;
  readonly download: { readonly url: string; readonly name: string } | null;
  readonly onShare: (() => void) | null;
  readonly onDelete: (() => void) | null;
}) {
  const wide = wall || onDelete !== null;
  if (!wide && file === null && onShare === null) return null;
  return (
    <span className="fd-art-more" data-wide={wide ? undefined : 'none'}>
      <Menu summary={<Icon of={Ellipsis} />} summaryClass="fd-iconbtn fd-sm" label="More actions">
        {(close) => (
          <>
            {file === null ? null : (
              <button
                type="button"
                className="fd-art-narrow"
                onClick={() => {
                  close();
                  download(file.url, file.name);
                }}
              >
                <Icon of={ArrowDown} /> Download
              </button>
            )}
            {onShare === null ? null : (
              <button
                type="button"
                className="fd-art-narrow"
                onClick={() => {
                  close();
                  onShare();
                }}
              >
                <Icon of={Share2} /> Share…
              </button>
            )}
            <SendToWall target={target} onOpen={close} />
            {wall && onDelete !== null ? <div className="fd-menu-sep" /> : null}
            {onDelete === null ? null : (
              <button
                type="button"
                className="fd-danger"
                onClick={() => {
                  close();
                  onDelete();
                }}
              >
                <Icon of={Trash2} /> {DELETE_LABEL}
              </button>
            )}
          </>
        )}
      </Menu>
    </span>
  );
}

/**
 * The version of the shown artifact: a quiet select while there are versions to pick (the ghost
 * view option of a page header, §4.3), else the one version as a muted badge ("v1": no select
 * with one option, no "(latest)").
 */
function VersionControl({
  ready,
  onVersion,
}: {
  readonly ready: ViewerReady;
  readonly onVersion: (version: number) => void;
}) {
  const { artifact, versions } = ready.detail;
  if (versions.length <= 1) {
    return (
      <span className="fd-badge fd-art-version" data-tone="muted">
        <span className="fd-vh">Version </span>v{ready.version}
      </span>
    );
  }
  return (
    <label className="fd-art-version">
      <span className="fd-vh">Version</span>
      <select
        className="fd-ghost fd-sm"
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
  );
}

/** "published 5 h ago", "published just now" (the age of the shown version, §5 times). */
function publishedText(at: number, now: number): string {
  const age = relativeAge(at, now);
  return age === 'now' ? 'published just now' : `published ${age} ago`;
}

/** The viewer without data calls: the toolbar and the body of one state. */
export function ArtifactViewerView({
  data,
  variant,
  canShare,
  onVersion,
  onBack,
  onShare,
  onRetry,
  deletion,
  sessions = [],
  now: givenNow,
}: ViewProps) {
  const wall = useSettings() !== undefined;
  const now = useNow(givenNow, 30_000);
  // §4.8: the phone top bar names the artifact ("Artifact" while it loads).
  usePhoneTitle(
    variant === 'page' && data.kind === 'ok' ? data.value.detail.artifact.title : undefined,
  );
  const page = variant === 'page';
  // The page has the Page header (§4.3: the way back above the title); the session panel keeps
  // its bar with a back button.
  const back = page ? null : (
    <button type="button" className="fd-ghost fd-sm fd-art-back" onClick={onBack}>
      <Icon of={ChevronLeft} size={14} /> Back to the list
    </button>
  );
  if (data.kind !== 'ok') {
    const state =
      data.kind === 'loading' ? (
        <Loading text="Loading the artifact…" />
      ) : (
        <ErrorNotice text={data.text} onRetry={onRetry} />
      );
    return page ? (
      <Page title="Artifact" back={ALL_ARTIFACTS} className="fd-art-pageview" label="Artifact" wide>
        {state}
      </Page>
    ) : (
      <section className="fd-art fd-art-viewer" aria-label="Artifact" data-variant={variant}>
        <div className="fd-art-bar">{back}</div>
        {state}
      </section>
    );
  }
  const ready = data.value;
  const { artifact } = ready.detail;
  const main = mainFile(ready.detail, ready.version);
  const viewUrl = ready.link.kind === 'ok' ? ready.link.value : null;
  const rawUrl =
    main === undefined
      ? null
      : rawPath(artifact.station_id, artifact.artifact_id, ready.version, main);
  const canDelete =
    deletion !== undefined && (deletion.phase.kind === 'idle' || deletion.phase.kind === 'error');
  const kind = (
    <span className="fd-badge" data-kind={artifact.kind}>
      {KIND_LABEL[artifact.kind]}
    </span>
  );
  const version = <VersionControl ready={ready} onVersion={onVersion} />;
  const actions = (
    <>
      {viewUrl === null ? null : (
        <a
          className="fd-btn fd-ghost fd-sm fd-art-action"
          href={viewUrl}
          target="_blank"
          rel="noopener noreferrer"
        >
          <Icon of={LinkIcon} size={14} /> Open in a new tab
        </a>
      )}
      {main === undefined || rawUrl === null ? null : (
        <a
          className="fd-btn fd-ghost fd-sm fd-art-action fd-art-wide"
          href={rawUrl}
          download={fileName(main)}
        >
          <Icon of={ArrowDown} size={14} /> Download
        </a>
      )}
      {canShare ? (
        <button
          type="button"
          className="fd-ghost fd-sm fd-art-action fd-art-wide"
          onClick={onShare}
        >
          <Icon of={Share2} size={14} /> Share…
        </button>
      ) : null}
      <MoreActions
        wall={wall}
        target={{
          kind: 'artifact',
          station_id: artifact.station_id,
          artifact_id: artifact.artifact_id,
          version: ready.version,
        }}
        download={
          main === undefined || rawUrl === null ? null : { url: rawUrl, name: fileName(main) }
        }
        onShare={canShare ? onShare : null}
        onDelete={canDelete ? (deletion?.onBegin ?? null) : null}
      />
    </>
  );
  const body = (
    <>
      {deletion === undefined ? null : <DeletionNotes deletion={deletion} />}
      <div className="fd-art-body" data-kind={artifact.kind}>
        {deletion?.phase.kind === 'hidden' ? (
          <p role="status" className="fd-note fd-art-state">
            {stateText(deletion.phase.state)}
          </p>
        ) : (
          <Body ready={ready} onRetry={onRetry} />
        )}
      </div>
    </>
  );
  if (page) {
    // The page header (§4.3): the way back above the title, then who published it and when; the
    // kind and the version are the filters of the header, the actions at the right.
    const card = sessions.find(
      (s) => s.station_id === artifact.station_id && s.summary.session_id === artifact.session_id,
    );
    const published = versionRow(ready.detail, ready.version)?.created_at ?? artifact.updated_at;
    // The dots of the summary keep to the word before them (a no-break space): a wrapped line
    // never starts with a dot.
    const where = [artifact.station_name, ...(card === undefined ? [] : [sessionTitle(card)])]
      .filter((part) => part !== '')
      .join(DOT);
    // A phone (artifacts.css) says the one version in the summary, where it needs no control of
    // its own ("Lighting · Review · v1 · published 5 h ago"); more versions keep the select.
    const rest = `${where === '' ? '' : DOT}${publishedText(published, now)}`;
    const summary =
      ready.detail.versions.length > 1 ? (
        `${where}${rest}`
      ) : (
        <>
          {where}
          <span className="fd-art-metaver">
            {where === '' ? `v${ready.version}${DOT}` : `${DOT}v${ready.version}`}
          </span>
          {rest}
        </>
      );
    return (
      <Page
        title={artifact.title}
        back={ALL_ARTIFACTS}
        description={summary}
        filters={
          <>
            {kind}
            {version}
          </>
        }
        actions={actions}
        className="fd-art-pageview"
        label="Artifact"
        wide
      >
        {body}
      </Page>
    );
  }
  return (
    <section className="fd-art fd-art-viewer" aria-label="Artifact" data-variant={variant}>
      <div className="fd-art-bar">
        {back}
        <h2 className="fd-art-name">{artifact.title}</h2>
        {kind}
        {version}
        {actions}
      </div>
      {body}
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
  /** The sessions of the board (the page names the session that published it). */
  readonly sessions?: readonly DeckSession[] | undefined;
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
  sessions,
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
        onRetry={() => setReload((n) => n + 1)}
        deletion={deletion}
        sessions={sessions}
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
  sessions,
}: {
  readonly actions: ArtifactActions | undefined;
  readonly station: string;
  readonly id: string;
  readonly version: number | null;
  readonly onVersion: (version: number) => void;
  /** The sessions of the board: the page header names the session that published it. */
  readonly sessions?: readonly DeckSession[] | undefined;
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
      sessions={sessions}
    />
  );
}
