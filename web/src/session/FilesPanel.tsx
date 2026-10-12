import { useState } from 'react';
import type { FileContent, FileListing, FileSearch } from '../gen';
import { type FileActions, joinPath, parentPath, type SearchModeName } from './workspace';

type View =
  | { readonly kind: 'idle' }
  | { readonly kind: 'loading' }
  | { readonly kind: 'error'; readonly text: string }
  | { readonly kind: 'listing'; readonly listing: FileListing }
  | { readonly kind: 'file'; readonly file: FileContent }
  | { readonly kind: 'search'; readonly search: FileSearch };

const message = (e: unknown) => (e instanceof Error ? e.message : 'The request failed.');

/**
 * The files below the session's directory (`files.v1`, DL-088), read only.
 * The station refuses paths outside the root, symlinks, hidden credential
 * directories and `.git`; the board shows its refusal text.
 */
export function FilesPanel({ files }: { readonly files: FileActions | null }) {
  const [view, setView] = useState<View>({ kind: 'idle' });
  const [query, setQuery] = useState('');
  const [mode, setMode] = useState<SearchModeName>('name');
  if (!files) return null;
  const load = <T,>(call: Promise<T>, done: (value: T) => View) => {
    setView({ kind: 'loading' });
    void call.then(
      (value) => setView(done(value)),
      (e: unknown) => setView({ kind: 'error', text: message(e) }),
    );
  };
  const open = (path: string) =>
    load(files.list(path), (listing) => ({ kind: 'listing', listing }));
  const read = (path: string) => load(files.read(path), (file) => ({ kind: 'file', file }));
  return (
    <section className="fd-files" aria-label="Files">
      <h3>Files</h3>
      <div className="fd-files-bar">
        <button type="button" onClick={() => open('')}>
          Browse
        </button>
        <form
          aria-label="Search the files"
          onSubmit={(event) => {
            event.preventDefault();
            load(files.search(query.trim(), mode), (search) => ({ kind: 'search', search }));
          }}
        >
          <label htmlFor="fd-file-query">Search</label>
          <input
            id="fd-file-query"
            value={query}
            minLength={2}
            maxLength={256}
            onChange={(event) => setQuery(event.target.value)}
          />
          <select
            aria-label="Search in"
            value={mode}
            onChange={(event) => setMode(event.target.value === 'content' ? 'content' : 'name')}
          >
            <option value="name">Names</option>
            <option value="content">Content</option>
          </select>
          <button type="submit" disabled={query.trim().length < 2}>
            Search
          </button>
        </form>
      </div>
      {view.kind === 'loading' ? <p role="status">Loading…</p> : null}
      {view.kind === 'error' ? <p role="alert">{view.text}</p> : null}
      {view.kind === 'listing' ? (
        <div data-listing={view.listing.path}>
          <p className="fd-path">/{view.listing.path}</p>
          <ul>
            {view.listing.path === '' ? null : (
              <li>
                <button
                  type="button"
                  className="fd-link"
                  onClick={() => open(parentPath(view.listing.path))}
                >
                  ..
                </button>
              </li>
            )}
            {view.listing.entries.map((e) => {
              const path = joinPath(view.listing.path, e.name);
              return (
                <li key={e.name} data-entry-kind={e.kind}>
                  {e.kind === 'dir' || e.kind === 'file' ? (
                    <button
                      type="button"
                      className="fd-link"
                      onClick={() => (e.kind === 'dir' ? open(path) : read(path))}
                    >
                      {e.kind === 'dir' ? `${e.name}/` : e.name}
                    </button>
                  ) : (
                    <span>
                      {e.name} ({e.kind}, not opened)
                    </span>
                  )}
                  {e.size !== null ? <span className="fd-size"> {e.size} B</span> : null}
                </li>
              );
            })}
          </ul>
          {view.listing.truncated ? <p className="fd-note">The list is cut at its limit.</p> : null}
        </div>
      ) : null}
      {view.kind === 'file' ? (
        <div data-file={view.file.path}>
          <p className="fd-path">
            /{view.file.path} · {view.file.size} B{' '}
            <button
              type="button"
              className="fd-link"
              onClick={() => open(parentPath(view.file.path))}
            >
              Back to the folder
            </button>
          </p>
          {view.file.binary ? (
            <p className="fd-note">A binary file is not shown.</p>
          ) : (
            <pre className="fd-file-text">{view.file.text}</pre>
          )}
          {view.file.truncated ? (
            <p className="fd-note">The file is cut at the size limit.</p>
          ) : null}
        </div>
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
