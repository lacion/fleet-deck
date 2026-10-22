import { useEffect, useRef, useState } from 'react';
import { useArtifacts } from '../artifacts/context';
import { publishedText, PublishDialog } from '../artifacts/PublishDialog';
import { artifactHref, publishable } from '../artifacts/kinds';
import { Markdown } from '../chat/Markdown';
import type { ArtifactPublished, FileContent, FileListing, FileSearch } from '../gen';
import { FileText, Folder, Icon, Upload } from '../ui/icons';
import { type ChangedFiles, loadChanged } from './changedFiles';
import { CopyButton } from './SessionHeader';
import { type FileActions, joinPath, parentPath, type SearchModeName } from './workspace';

type View =
  | { readonly kind: 'idle' }
  | { readonly kind: 'loading' }
  | { readonly kind: 'error'; readonly text: string }
  | { readonly kind: 'listing'; readonly listing: FileListing }
  | { readonly kind: 'file'; readonly file: FileContent }
  | { readonly kind: 'search'; readonly search: FileSearch };

/** `812` → `812 B`, `3200` → `3.1 KB`, `1250000` → `1.2 MB` (one decimal from a kilobyte on). */
export function sizeText(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

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
          return (
            <li key={path}>
              <button type="button" className="fd-files-row" onClick={() => onOpen(path)}>
                <Icon of={FileText} size={14} />
                <span className="fd-files-name">{path.split('/').pop()}</span>
                {dir === '' ? null : <span className="fd-files-dir">{dir}</span>}
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
}: {
  readonly files: FileActions | null;
  readonly variant?: 'tab' | 'side';
  /** Opens `path` (in its folder) each time `seq` changes. */
  readonly request?: { readonly path: string; readonly seq: number } | undefined;
  /** The session directory; "Copy the path" puts it before the path. */
  readonly root?: string | null;
}) {
  const [view, setView] = useState<View>(files ? { kind: 'loading' } : { kind: 'idle' });
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
    setView({ kind: 'loading' });
    void current.list('').then(
      (listing) => setView({ kind: 'listing', listing }),
      (e: unknown) => setView({ kind: 'error', text: message(e) }),
    );
  }, [has]);
  const [changed, setChanged] = useState<ChangedFiles | null>(null);
  useEffect(() => {
    const current = latest.current;
    if (!has || !current) return;
    let live = true;
    void loadChanged(current).then((found) => {
      if (live) setChanged(found);
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
    setView({ kind: 'loading' });
    void current.read(wanted.path).then(
      (file) => setView({ kind: 'file', file }),
      (e: unknown) => setView({ kind: 'error', text: message(e) }),
    );
  }, [seq]);
  if (!files) return null;
  const load = <T,>(call: Promise<T>, done: (value: T) => View) => {
    setView({ kind: 'loading' });
    void call.then(
      (value) => setView(done(value)),
      (e: unknown) => setView({ kind: 'error', text: message(e) }),
    );
  };
  const open = (path: string) => {
    setFolder(path);
    load(files.list(path), (listing) => ({ kind: 'listing', listing }));
  };
  const read = (path: string) => {
    setFolder(parentPath(path));
    load(files.read(path), (file) => ({ kind: 'file', file }));
  };
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
            load(files.search(query.trim(), mode), (search) => ({ kind: 'search', search }));
          }}
        >
          <label htmlFor="fd-file-query" className="fd-vh">
            Search
          </label>
          <input
            id="fd-file-query"
            className="fd-sm"
            placeholder="Search files"
            value={query}
            minLength={2}
            maxLength={256}
            onChange={(event) => setQuery(event.target.value)}
          />
          <select
            className="fd-sm"
            aria-label="Search in"
            value={mode}
            onChange={(event) => setMode(event.target.value === 'content' ? 'content' : 'name')}
          >
            <option value="name">Names</option>
            <option value="content">Content</option>
          </select>
          <button type="submit" className="fd-sm" disabled={query.trim().length < 2}>
            Search
          </button>
        </form>
      </div>
      {changed === null ? null : <ChangedFilesList changed={changed} onOpen={read} />}
      <nav className="fd-files-crumbs" aria-label="Folder path">
        <button type="button" className="fd-files-crumb" onClick={() => open('')}>
          <Icon of={Folder} size={14} /> root
        </button>
        {segments.map((segment, n) => {
          const path = segments.slice(0, n + 1).join('/');
          return (
            <span key={path} className="fd-files-crumb-item">
              <span className="fd-files-sep" aria-hidden="true">
                /
              </span>
              <button type="button" className="fd-files-crumb" onClick={() => open(path)}>
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
      {view.kind === 'loading' ? (
        <p role="status" className="fd-note">
          Loading…
        </p>
      ) : null}
      {view.kind === 'error' ? (
        <p role="alert" className="fd-files-error">
          {view.text}
        </p>
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
              return (
                <li
                  key={e.name}
                  data-entry-kind={e.kind}
                  className={canPublish ? 'fd-files-item' : undefined}
                >
                  {e.kind === 'dir' || e.kind === 'file' ? (
                    <button
                      type="button"
                      className="fd-files-row"
                      onClick={() => (e.kind === 'dir' ? open(path) : read(path))}
                    >
                      <Icon of={e.kind === 'dir' ? Folder : FileText} size={14} />
                      <span className="fd-files-name">
                        {e.kind === 'dir' ? `${e.name}/` : e.name}
                      </span>
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
                  ) : canPublish ? (
                    <span className="fd-files-publish" aria-hidden="true" />
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
          {view.search.hits.length === 0 ? <p>No match.</p> : null}
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
