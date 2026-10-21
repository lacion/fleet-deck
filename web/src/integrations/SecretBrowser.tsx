// Fleet Sync: the "Browse…" panel of the Secret files section. It lists one folder of a home on a
// station, hidden entries included, and adds a file that can be synced to the list with one click.
// It is presentational: `SecretFiles` holds the state and makes the calls.
import type { SecretBrowseAnswer, SecretBrowseEntry } from '../gen';
import { Folder, Icon } from '../ui/icons';

type BrowseHome = {
  readonly station_id: string;
  readonly name: string;
  readonly path: string;
};

const REASONS: Readonly<Record<string, string>> = {
  not_ignored: 'Git does not ignore it',
  not_member_hidden: 'Members can read this name',
  too_large: 'Larger than 512 KiB',
  symlink: 'A link',
  private_file: 'A private file of the station',
  not_regular: 'Not a regular file',
  invalid_path: 'Not a valid secret file path',
  unreadable: 'Cannot be read',
};

/** The path of the entry `name` of the folder `dir` (`''` is the home folder), without a leading `/`. */
export function browsePath(dir: string, name: string): string {
  return dir === '' ? name : `${dir}/${name}`;
}

/** Why a file cannot be synced: the text of its reason code; an unknown code shows as it is. */
export function reasonText(code: string): string {
  return REASONS[code] ?? code;
}

/** "12 B", "1.2 KiB", "3.4 MiB". */
function sizeText(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
}

/** The last segment of a folder path. */
function folderName(path: string): string {
  return (
    path
      .split('/')
      .filter((part) => part !== '')
      .pop() ?? path
  );
}

function inDraft(draft: string, path: string): boolean {
  return draft.split('\n').some((line) => line.trim() === path);
}

function EntryRow({
  entry,
  dir,
  draft,
  busy,
  onDir,
  onAdd,
}: {
  readonly entry: SecretBrowseEntry;
  readonly dir: string;
  readonly draft: string;
  readonly busy: boolean;
  readonly onDir: (dir: string) => void;
  readonly onAdd: (path: string) => void;
}) {
  const path = browsePath(dir, entry.name);
  if (entry.kind === 'dir') {
    return (
      <li className="fd-secret-entry">
        <button
          type="button"
          aria-label={`Open ${entry.name}`}
          disabled={busy}
          onClick={() => onDir(path)}
        >
          <Icon of={Folder} size={14} />
          {entry.name}
        </button>
      </li>
    );
  }
  const size =
    entry.size === undefined ? null : <span className="fd-note">{sizeText(entry.size)}</span>;
  if (entry.kind === 'file' && entry.eligible) {
    const added = inDraft(draft, path);
    return (
      <li className="fd-secret-entry">
        <span className="fd-secret-name">{entry.name}</span>
        {size}
        <button type="button" className="fd-ghost" disabled={added} onClick={() => onAdd(path)}>
          {added ? 'Added' : 'Add'}
        </button>
      </li>
    );
  }
  return (
    <li className="fd-secret-entry" data-muted="true">
      <span className="fd-secret-name">{entry.name}</span>
      {size}
      <span className="fd-note">{reasonText(entry.reason ?? entry.kind)}</span>
    </li>
  );
}

type Props = {
  readonly homes: readonly BrowseHome[];
  readonly station: string;
  readonly answer: SecretBrowseAnswer | null;
  readonly error: string | null;
  readonly busy: boolean;
  readonly draft: string;
  readonly onStation: (id: string) => void;
  readonly onDir: (dir: string) => void;
  readonly onAdd: (path: string) => void;
  readonly onClose: () => void;
};

/** The panel: the station, the crumbs, the note, one row for each entry and "Close". */
export function SecretBrowser({
  homes,
  station,
  answer,
  error,
  busy,
  draft,
  onStation,
  onDir,
  onAdd,
  onClose,
}: Props) {
  const home = homes.find((h) => h.station_id === station);
  const dir = answer?.dir ?? '';
  const parts = dir === '' ? [] : dir.split('/');
  return (
    <div className="fd-secret-browse" role="region" aria-label="Browse the home">
      <label>
        Station
        <select value={station} onChange={(e) => onStation(e.target.value)}>
          {homes.map((h) => (
            <option key={h.station_id} value={h.station_id}>
              {h.name}
            </option>
          ))}
        </select>
      </label>
      <nav aria-label="Folder path">
        <ol className="fd-secret-crumbs">
          <li>
            <button
              type="button"
              className="fd-ghost"
              aria-current={parts.length === 0 ? 'location' : undefined}
              disabled={busy}
              onClick={() => onDir('')}
            >
              {home === undefined ? 'Home' : folderName(home.path)}
            </button>
          </li>
          {parts.map((part, i) => (
            <li key={parts.slice(0, i + 1).join('/')}>
              <button
                type="button"
                className="fd-ghost"
                aria-current={i === parts.length - 1 ? 'location' : undefined}
                disabled={busy}
                onClick={() => onDir(parts.slice(0, i + 1).join('/'))}
              >
                {part}
              </button>
            </li>
          ))}
        </ol>
      </nav>
      <p className="fd-note">
        Hidden files are shown. Only files that Git ignores and that members cannot read can be
        synced.
      </p>
      {busy ? (
        <p role="status" className="fd-note">
          Loading…
        </p>
      ) : null}
      {answer === null ? null : answer.entries.length === 0 ? (
        <p className="fd-note">This folder is empty.</p>
      ) : (
        <ul className="fd-secret-entries">
          {answer.entries.map((entry) => (
            <EntryRow
              key={entry.name}
              entry={entry}
              dir={answer.dir}
              draft={draft}
              busy={busy}
              onDir={onDir}
              onAdd={onAdd}
            />
          ))}
        </ul>
      )}
      {answer?.truncated === true ? (
        <p className="fd-note">Only the first 500 entries are shown.</p>
      ) : null}
      {error === null ? null : (
        <p role="alert" className="fd-formerror">
          {error}
        </p>
      )}
      <div className="fd-pane-row">
        <button type="button" className="fd-ghost" onClick={onClose}>
          Close
        </button>
      </div>
    </div>
  );
}
