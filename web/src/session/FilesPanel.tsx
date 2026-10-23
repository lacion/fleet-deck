import { useEffect, useRef, useState } from 'react';
import { useArtifacts } from '../artifacts/context';
import { publishedText, PublishDialog } from '../artifacts/PublishDialog';
import { artifactHref, publishable } from '../artifacts/kinds';
import { fileStatusBadge } from '../chat/Diff';
import { Markdown } from '../chat/Markdown';
import { PathTail } from '../chat/PathTail';
import type { ArtifactPublished, FileContent, FileListing, FileSearch } from '../gen';
import { addedText, removedText, sizeText } from '../ui/format';
import { FileText, Folder, Icon, Search, Upload } from '../ui/icons';
import { Loading } from '../ui/Loading';
import { ErrorNotice } from '../ui/page';
import {
  type ChangedFiles,
  type ChangeKind,
  type FileChange,
  loadSessionChanges,
} from './changedFiles';
import { CopyButton } from './SessionHeader';
import {
  type FileActions,
  joinPath,
  replaceDiffPaths,
  parentPath,
  type SearchModeName,
} from './workspace';

/** A read of the panel, so that "Try again" after a failed read runs the same read again. */
type Read =
  | { readonly kind: 'list'; readonly path: string }
  | { readonly kind: 'file'; readonly path: string }
  | { readonly kind: 'search'; readonly query: string; readonly mode: SearchModeName };

type View =
  | { readonly kind: 'idle' }
  | { readonly kind: 'loading'; readonly read: Read['kind'] }
  | { readonly kind: 'error'; readonly text: string; readonly read: Read }
  | { readonly kind: 'listing'; readonly listing: FileListing }
  | { readonly kind: 'file'; readonly file: FileContent }
  | { readonly kind: 'search'; readonly search: FileSearch; readonly query: string };

/** The loading line of each read (§4.7 `Loading`). */
const LOADING: Record<Read['kind'], string> = {
  list: 'Loading the folder…',
  file: 'Loading the file…',
  search: 'Searching the files…',
};

/** A Markdown file shows rendered or raw; a path copy is always there. */
export function FileView({
  file,
  root,
  onBack,
  onPublish,
}: {
  readonly file: FileContent;
  readonly root: string | null;
  readonly onBack: () => void;
  /** "Publish as artifact" (R12); absent: the button is not shown. */
  readonly onPublish?: (() => void) | undefined;
}) {
  const [rendered, setRendered] = useState(true);
  const markdown = /\.(md|markdown)$/i.test(file.path);
  const showRendered = markdown && rendered && !file.binary;
  return (
    <div data-file={file.path}>
      <p className="fd-path">
        {sizeText(file.size)}{' '}
        <button type="button" className="fd-link" onClick={onBack}>
          Back to the folder
        </button>
      </p>
      <div className="fd-files-tools">
        {markdown ? (
          <>
            <button
              type="button"
              className="fd-chip"
              aria-pressed={rendered}
              onClick={() => setRendered(true)}
            >
              Rendered
            </button>
            <button
              type="button"
              className="fd-chip"
              aria-pressed={!rendered}
              onClick={() => setRendered(false)}
            >
              Raw
            </button>
          </>
        ) : null}
        <CopyButton text={root ? `${root}/${file.path}` : file.path} label="Copy the path" />
        {onPublish !== undefined && publishable(file.path) ? (
          <button type="button" className="fd-ghost fd-sm" onClick={onPublish}>
            <Icon of={Upload} size={14} /> Publish as artifact
          </button>
        ) : null}
      </div>
      {file.binary ? (
        <p className="fd-note">A binary file is not shown.</p>
      ) : showRendered ? (
        <div className="fd-files-md">
          <Markdown text={file.text ?? ''} />
        </div>
      ) : (
        <pre className="fd-code fd-file-text">{file.text}</pre>
      )}
      {file.truncated ? <p className="fd-note">The file is cut at the size limit.</p> : null}
    </div>
  );
}

const KIND_NAME: Record<ChangeKind, string> = { A: 'added', M: 'modified', R: 'renamed' };

/**
 * The badge of a change kind, the same as the status badge of a diff file header (§3 `.fd-diff`):
 * none for a modified file, an ok "new" for an added file, a neutral word for a renamed one. (A
 * deleted file is not in the list: it cannot be opened.)
 */
const KIND_BADGE: Record<ChangeKind, ReturnType<typeof fileStatusBadge> | null> = {
  A: fileStatusBadge({ kind: 'added' }),
  M: null,
  R: { text: 'renamed', title: 'Renamed file', tone: undefined },
};

/**
 * The change of one file at the right of its row: the badge of its kind, then `+9 −3` (the order
 * of a diff file header, so the counts end on one edge).
 */
function ChangeStat({ change }: { readonly change: FileChange }) {
  const badge = KIND_BADGE[change.kind];
  return (
    <span className="fd-files-stat">
      {badge === null ? (
        <span className="fd-vh">{KIND_NAME[change.kind]}</span>
      ) : (
        <span className="fd-badge" data-tone={badge.tone} title={badge.title}>
          <span aria-hidden="true">{badge.text}</span>
          <span className="fd-vh">{KIND_NAME[change.kind]}</span>
        </span>
      )}
      {change.insertions > 0 ? (
        <span className="fd-add">{addedText(change.insertions)}</span>
      ) : null}
      {change.deletions > 0 ? (
        <span className="fd-del">{removedText(change.deletions)}</span>
      ) : null}
    </span>
  );
}

/** The files changed since the session started; a click opens the file. */
export function ChangedFilesList({
  changed,
  onOpen,
}: {
  readonly changed: ChangedFiles;
  readonly onOpen: (path: string) => void;
}) {
  return (
    <section className="fd-files-changed" aria-label="Changed files">
      <h4 className="fd-label">Changed since the session started</h4>
      <ul>
        {changed.paths.map((path) => {
          const dir = parentPath(path);
          const change = changed.changes[path];
          return (
            <li key={path}>
              <button type="button" className="fd-files-row" onClick={() => onOpen(path)}>
                <Icon of={FileText} size={14} />
                <span className="fd-files-name">{path.split('/').pop()}</span>
                {/* Whole folders give way ("…/e2e"): a cut never lands inside a name. */}
                {dir === '' ? null : (
                  <span className="fd-files-dir">
                    <PathTail path={dir} />
                  </span>
                )}
                {change === undefined ? null : <ChangeStat change={change} />}
              </button>
            </li>
          );
        })}
      </ul>
      {changed.truncated ? <p className="fd-note">More changes than shown.</p> : null}
    </section>
  );
}

const message = (e: unknown) => (e instanceof Error ? e.message : 'The request failed.');

/** The last name of a folder path (`~/projects/fleet-deck` → `fleet-deck`, `/` → `/`). */
export function folderName(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, '');
  return trimmed.split(/[\\/]/).pop() || path;
}

/**
 * The files below the session's directory (`files.v1`, DL-088), read only.
 * The station refuses paths outside the root, symlinks, hidden credential
 * directories and `.git`; the board shows its refusal text.
 */
export function FilesPanel({
  files,
  variant = 'tab',
  request,
  root = null,
  diffRef,
}: {
  readonly files: FileActions | null;
  readonly variant?: 'tab' | 'side';
  /** Opens `path` (in its folder) each time `seq` changes. */
  readonly request?: { readonly path: string; readonly seq: number } | undefined;
  /** The session directory; "Copy the path" puts it before the path. */
  readonly root?: string | null;
  /** The session (`<station>/<session>`) whose Properties panel also lists the changed files. */
  readonly diffRef?: string | undefined;
}) {
  const [view, setView] = useState<View>(
    files ? { kind: 'loading', read: 'list' } : { kind: 'idle' },
  );
  /** The last requested folder; the crumbs show it. */
  const [folder, setFolder] = useState('');
  const [query, setQuery] = useState('');
  const [mode, setMode] = useState<SearchModeName>('name');
  // R12: "Publish as artifact" is there for a session's files when the login can drive.
  const artifacts = useArtifacts();
  const canPublish = artifacts?.actions.canPublish === true && artifacts.session !== undefined;
  const [publishing, setPublishing] = useState<string | null>(null);
  const [published, setPublished] = useState<ArtifactPublished | null>(null);
  // The actions object changes on every render of the parent; the effect keys on "has files".
  const latest = useRef(files);
  latest.current = files;
  const has = files !== null;
  const requested = useRef(request);
  requested.current = request;
  useEffect(() => {
    const current = latest.current;
    if (!has || !current) return;
    // A request that is there at the start opens its file instead of the root.
    if (requested.current) return;
    setFolder('');
    setView({ kind: 'loading', read: 'list' });
    void current.list('').then(
      (listing) => setView({ kind: 'listing', listing }),
      (e: unknown) =>
        setView({ kind: 'error', text: message(e), read: { kind: 'list', path: '' } }),
    );
  }, [has]);
  const [changed, setChanged] = useState<ChangedFiles | null>(null);
  const changedRef = useRef(diffRef);
  changedRef.current = diffRef;
  useEffect(() => {
    const current = latest.current;
    if (!has || !current) return;
    let live = true;
    void loadSessionChanges(current).then((found) => {
      if (!live) return;
      setChanged(found?.changed ?? null);
      if (found !== null && changedRef.current !== undefined) {
        replaceDiffPaths(changedRef.current, found.all);
      }
    });
    return () => {
      live = false;
    };
  }, [has]);
  const seq = request?.seq;
  useEffect(() => {
    const current = latest.current;
    const wanted = requested.current;
    if (seq === undefined || !wanted || !current) return;
    setFolder(parentPath(wanted.path));
    setView({ kind: 'loading', read: 'file' });
    void current.read(wanted.path).then(
      (file) => setView({ kind: 'file', file }),
      (e: unknown) =>
        setView({ kind: 'error', text: message(e), read: { kind: 'file', path: wanted.path } }),
    );
  }, [seq]);
  if (!files) return null;
  const load = <T,>(call: Promise<T>, done: (value: T) => View, what: Read) => {
    setView({ kind: 'loading', read: what.kind });
    void call.then(
      (value) => setView(done(value)),
      (e: unknown) => setView({ kind: 'error', text: message(e), read: what }),
    );
  };
  const open = (path: string) => {
    setFolder(path);
    load(files.list(path), (listing) => ({ kind: 'listing', listing }), { kind: 'list', path });
  };
  const read = (path: string) => {
    setFolder(parentPath(path));
    load(files.read(path), (file) => ({ kind: 'file', file }), { kind: 'file', path });
  };
  const search = (text: string, how: SearchModeName) =>
    load(files.search(text, how), (found) => ({ kind: 'search', search: found, query: text }), {
      kind: 'search',
      query: text,
      mode: how,
    });
  const again = (what: Read) =>
    what.kind === 'list'
      ? open(what.path)
      : what.kind === 'file'
        ? read(what.path)
        : search(what.query, what.mode);
  const segments = folder === '' ? [] : folder.split('/');
  const fileName = view.kind === 'file' ? (view.file.path.split('/').pop() ?? '') : null;
  return (
    <section className="fd-files" aria-label="Files" data-variant={variant}>
      <h3 className="fd-vh">Files</h3>
      <div className="fd-files-bar">
        <form
          aria-label="Search the files"
          onSubmit={(event) => {
            event.preventDefault();
            search(query.trim(), mode);
          }}
        >
          <label htmlFor="fd-file-query" className="fd-vh">
            Search
          </label>
          {/* The search icon in the field is the submit button (Enter submits too). */}
          <div className="fd-field fd-files-query">
            <input
              id="fd-file-query"
              className="fd-sm"
              placeholder="Search files"
              value={query}
              minLength={2}
              maxLength={256}
              onChange={(event) => setQuery(event.target.value)}
            />
            <button type="submit" className="fd-files-go" disabled={query.trim().length < 2}>
              <Icon of={Search} />
              <span className="fd-vh">Search</span>
            </button>
          </div>
          {/* What the search reads: the file names or their content (a choice, not a filter). */}
          <div className="fd-seg fd-files-mode" role="group" aria-label="Search in">
            {(['name', 'content'] as const).map((m) => (
              <button key={m} type="button" aria-pressed={mode === m} onClick={() => setMode(m)}>
                {m === 'name' ? 'Names' : 'Content'}
              </button>
            ))}
          </div>
        </form>
      </div>
      {changed === null ? null : <ChangedFilesList changed={changed} onOpen={read} />}
      <nav className="fd-files-crumbs" aria-label="Folder path">
        {/* The label of the listing, as "Changed since the session started" labels the changed
            files: "Files in", then the crumbs (sans, as the Browse crumbs) with a "/" between two
            of them; the current one is --text weight 600. */}
        <span className="fd-files-crumbs-lead">Files in</span>
        <span className="fd-files-crumb-item">
          <button
            type="button"
            className="fd-files-crumb fd-files-root"
            aria-current={segments.length === 0 && fileName === null ? 'location' : undefined}
            title={root ?? undefined}
            onClick={() => open('')}
          >
            {root ? (
              <>
                {folderName(root)}
                <span className="fd-vh">, root folder</span>
              </>
            ) : (
              'the root folder'
            )}
          </button>
        </span>
        {segments.map((segment, n) => {
          const path = segments.slice(0, n + 1).join('/');
          const here = n === segments.length - 1 && fileName === null;
          return (
            <span key={path} className="fd-files-crumb-item">
              <span className="fd-files-sep" aria-hidden="true">
                /
              </span>
              <button
                type="button"
                className="fd-files-crumb"
                aria-current={here ? 'location' : undefined}
                onClick={() => open(path)}
              >
                {segment}
              </button>
            </span>
          );
        })}
        {fileName !== null ? (
          <span className="fd-files-crumb-item">
            <span className="fd-files-sep" aria-hidden="true">
              /
            </span>
            <span className="fd-files-file">{fileName}</span>
          </span>
        ) : null}
      </nav>
      {published === null || artifacts?.session === undefined ? null : (
        <p role="status" className="fd-note fd-files-published">
          {publishedText(published)}{' '}
          <a
            href={artifactHref(
              artifacts.session.station,
              published.artifact.artifact_id,
              published.version.version,
            )}
          >
            Open it
          </a>
        </p>
      )}
      {publishing === null ? null : (
        <PublishDialog
          path={publishing}
          onClose={() => setPublishing(null)}
          onPublished={(done) => {
            setPublishing(null);
            setPublished(done);
          }}
        />
      )}
      {view.kind === 'loading' ? <Loading text={LOADING[view.read]} /> : null}
      {view.kind === 'error' ? (
        <ErrorNotice text={view.text} onRetry={() => again(view.read)} />
      ) : null}
      {view.kind === 'listing' ? (
        <div data-listing={view.listing.path}>
          <ul>
            {view.listing.path === '' ? null : (
              <li>
                <button
                  type="button"
                  className="fd-files-row"
                  onClick={() => open(parentPath(view.listing.path))}
                >
                  <Icon of={Folder} size={14} />
                  <span className="fd-files-name">..</span>
                </button>
              </li>
            )}
            {view.listing.entries.map((e) => {
              const path = joinPath(view.listing.path, e.name);
              const size =
                e.size !== null ? <span className="fd-size"> {sizeText(e.size)}</span> : null;
              const offer = e.kind === 'file' && canPublish && publishable(e.name);
              // A file that Publish offers: the button sits before the size (session.css lays
              // the row out as a grid), so that the sizes end on the edge of the toolbar.
              return (
                <li
                  key={e.name}
                  data-entry-kind={e.kind}
                  className={offer ? 'fd-files-item' : undefined}
                >
                  {e.kind === 'dir' || e.kind === 'file' ? (
                    // The folder icon marks a folder: its name has no trailing "/".
                    <button
                      type="button"
                      className="fd-files-row"
                      title={root ? `${root}/${path}` : path}
                      onClick={() => (e.kind === 'dir' ? open(path) : read(path))}
                    >
                      <Icon of={e.kind === 'dir' ? Folder : FileText} size={14} />
                      <span className="fd-files-name">{e.name}</span>
                      {size}
                    </button>
                  ) : (
                    <div className="fd-files-row" data-disabled>
                      <Icon of={FileText} size={14} />
                      <span className="fd-files-name">
                        {e.name} ({e.kind}, not opened)
                      </span>
                      {size}
                    </div>
                  )}
                  {offer ? (
                    <button
                      type="button"
                      className="fd-iconbtn fd-sm fd-files-publish"
                      aria-label={`Publish ${e.name} as artifact`}
                      title="Publish as artifact"
                      onClick={() => setPublishing(path)}
                    >
                      <Icon of={Upload} size={14} />
                    </button>
                  ) : null}
                </li>
              );
            })}
          </ul>
          {view.listing.entries.length === 0 ? (
            <p className="fd-empty">This folder is empty.</p>
          ) : null}
          {view.listing.truncated ? <p className="fd-note">The list is cut at its limit.</p> : null}
        </div>
      ) : null}
      {view.kind === 'file' ? (
        <FileView
          key={view.file.path}
          file={view.file}
          root={root}
          onBack={() => open(parentPath(view.file.path))}
          onPublish={canPublish ? () => setPublishing(view.file.path) : undefined}
        />
      ) : null}
      {view.kind === 'search' ? (
        <div data-search={view.search.backend}>
          {view.search.hits.length === 0 ? (
            <p className="fd-empty">No file matches “{view.query}”.</p>
          ) : null}
          <ul>
            {view.search.hits.map((h) => (
              <li key={`${h.path}:${h.line ?? ''}`}>
                <button type="button" className="fd-link" onClick={() => read(h.path)}>
                  {h.path}
                  {h.line !== null ? `:${h.line}` : ''}
                </button>
                {h.text ? <code> {h.text}</code> : null}
              </li>
            ))}
          </ul>
          {view.search.truncated ? (
            <p className="fd-note">There are more matches than the answer shows.</p>
          ) : null}
        </div>
      ) : null}
    </section>
  );
}
