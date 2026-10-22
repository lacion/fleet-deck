// DL-171: the folder of a station: a text input and "Browse…", which opens an inline panel with the
// breadcrumbs of the folder, its sub-folders, the pinned and recent folders, and "Use this folder".
// The spawn form, the Projects form and the Automations form use it.
import { useEffect, useRef, useState } from 'react';
import type { DeckSession, WorkspaceListing } from '../gen';
import { lastParts, readPins, recentDirs, togglePin, writePins } from '../spawn/spawn';
import { Folder, FolderGit2, Icon, Pin, PinOff } from './icons';

/** Reads one folder of a station; `null` is the list of the allowed roots. */
export type ListWorkspaces = (station: string, path: string | null) => Promise<WorkspaceListing>;

type Props = {
  readonly stationId: string;
  readonly value: string;
  readonly onChange: (path: string) => void;
  /** Reads one folder of the station; absent: the picker is a text input only. */
  readonly list: ListWorkspaces | undefined;
  /** The name of the input (the field label). */
  readonly label: string;
  /** The sessions of the fleet, for the recent folders of the station. */
  readonly sessions?: readonly DeckSession[] | undefined;
  /** Browse is off (the board is not live, no drive scope): the page says why. */
  readonly browseDisabled?: boolean;
  readonly placeholder?: string | undefined;
  /** The path is wrong (for example relative): the input is marked invalid. */
  readonly invalid?: boolean | undefined;
  /** The id of the note that says what the path needs. */
  readonly describedBy?: string | undefined;
  /** The panel is open at the first render (tests). */
  readonly initialOpen?: boolean;
  /** The listing at the first render (tests). */
  readonly initialListing?: WorkspaceListing | null;
};

/** The parts of an absolute path with the path up to each part: `/a/b` gives `a`→`/a`, `b`→`/a/b`. */
export function crumbsOf(
  path: string,
): readonly { readonly name: string; readonly path: string }[] {
  const parts = path.split('/').filter((p) => p !== '');
  return parts.map((name, i) => ({ name, path: `/${parts.slice(0, i + 1).join('/')}` }));
}

const message = (e: unknown) => (e instanceof Error ? e.message : 'The station did not answer.');

export function FolderPicker({
  stationId,
  value,
  onChange,
  list,
  label,
  sessions = [],
  browseDisabled = false,
  placeholder = '/home/me/projects/app',
  invalid,
  describedBy,
  initialOpen = false,
  initialListing = null,
}: Props) {
  const [open, setOpen] = useState(initialOpen);
  const [listing, setListing] = useState<WorkspaceListing | null>(initialListing);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [pins, setPins] = useState(readPins);
  // The first folder of the allowed roots that was listed: the crumbs above it are not offered.
  const [rootPath, setRootPath] = useState<string | null>(null);
  const turn = useRef(0);
  const station = useRef(stationId);
  const path = value.trim();
  const pinned = pins[stationId] ?? [];
  const recent = recentDirs(sessions, stationId, pinned);

  // A folder belongs to its station: another station closes the panel.
  useEffect(() => {
    if (station.current === stationId) return;
    station.current = stationId;
    turn.current += 1;
    setOpen(false);
    setListing(null);
    setRootPath(null);
    setError(null);
  }, [stationId]);

  const browse = (at: string | null, fallback = false) => {
    if (list === undefined) return;
    const mine = ++turn.current;
    setBusy(true);
    setError(null);
    list(stationId, at).then(
      (next) => {
        if (mine !== turn.current) return;
        setBusy(false);
        setListing(next);
        if (next.path !== null && next.parent === null) setRootPath(next.path);
        if (next.path === null) setRootPath(null);
      },
      (e: unknown) => {
        if (mine !== turn.current) return;
        setBusy(false);
        if (fallback && at !== null) browse(null);
        else setError(message(e));
      },
    );
  };
  const toggle = () => {
    if (open) {
      setOpen(false);
      return;
    }
    setOpen(true);
    if (listing === null) browse(path.startsWith('/') ? path : null, path.startsWith('/'));
  };
  // A pinned or recent folder is the choice: the panel closes, as after "Use this folder".
  const pick = (next: string) => {
    onChange(next);
    setOpen(false);
  };
  const crumbs = listing?.path == null ? [] : crumbsOf(listing.path);
  // A crumb above the allowed roots would be refused by the station: it is plain text.
  const offered = (p: string) =>
    p === listing?.parent ||
    (rootPath !== null && (p === rootPath || p.startsWith(`${rootPath}/`)));

  return (
    <div className="fd-folder">
      <div className="fd-folder-row">
        <input
          type="text"
          aria-label={label}
          aria-invalid={invalid ? true : undefined}
          aria-describedby={describedBy}
          value={value}
          spellCheck={false}
          autoComplete="off"
          placeholder={placeholder}
          onChange={(e) => onChange(e.target.value)}
        />
        {path === '' ? null : (
          <button
            type="button"
            className="fd-iconbtn"
            aria-label={pinned.includes(path) ? 'Unpin this directory' : 'Pin this directory'}
            title={pinned.includes(path) ? 'Unpin this directory' : 'Pin this directory'}
            onClick={() => {
              const next = togglePin(pins, stationId, path);
              setPins(next);
              writePins(next);
            }}
          >
            <Icon of={pinned.includes(path) ? PinOff : Pin} />
          </button>
        )}
        {list === undefined ? null : (
          <button
            type="button"
            aria-expanded={open}
            disabled={browseDisabled || stationId === ''}
            onClick={toggle}
          >
            <Icon of={Folder} /> Browse…
          </button>
        )}
      </div>
      {open && list !== undefined ? (
        <div className="fd-folder-panel" role="region" aria-label="Browse the station">
          {pinned.length === 0 && recent.length === 0 ? null : (
            <div className="fd-folder-quick fd-recent" aria-label="Recent directories">
              <span className="fd-label">{pinned.length > 0 ? 'Pinned and recent' : 'Recent'}</span>
              {pinned.map((p) => (
                <button
                  key={`pin:${p}`}
                  type="button"
                  className="fd-chip"
                  data-pinned
                  title={p}
                  onClick={() => pick(p)}
                >
                  <Icon of={Pin} size={12} />
                  {lastParts(p)}
                </button>
              ))}
              {recent.map((p) => (
                <button key={p} type="button" className="fd-chip" title={p} onClick={() => pick(p)}>
                  {lastParts(p)}
                </button>
              ))}
            </div>
          )}
          {listing === null ? (
            <p className="fd-note" role="status">
              {busy ? 'Loading…' : null}
            </p>
          ) : (
            <>
              <nav aria-label="Folder path">
                <ol className="fd-folder-crumbs">
                  <li>
                    <button type="button" className="fd-ghost" onClick={() => browse(null)}>
                      Roots
                    </button>
                  </li>
                  {crumbs.map((c, i) => (
                    <li key={c.path}>
                      {i === crumbs.length - 1 || !offered(c.path) ? (
                        <span aria-current={i === crumbs.length - 1 ? 'location' : undefined}>
                          {c.name}
                        </span>
                      ) : (
                        <button type="button" className="fd-ghost" onClick={() => browse(c.path)}>
                          {c.name}
                        </button>
                      )}
                    </li>
                  ))}
                </ol>
              </nav>
              {listing.entries.length === 0 ? (
                <p className="fd-note">
                  {listing.path === null ? 'No folders.' : 'No sub-directories.'}
                </p>
              ) : (
                <ul className="fd-folder-list">
                  {listing.entries.map((entry) => (
                    <li key={entry.path}>
                      <button
                        type="button"
                        aria-label={`Open ${entry.name}`}
                        onClick={() => browse(entry.path)}
                      >
                        <Icon of={entry.repository ? FolderGit2 : Folder} size={14} />
                        {entry.name}
                        {entry.repository ? <span className="fd-vh">git repository</span> : null}
                      </button>
                    </li>
                  ))}
                </ul>
              )}
              {listing.truncated ? (
                <p className="fd-note">The station shows only the first entries.</p>
              ) : null}
            </>
          )}
          {error === null ? null : (
            <p className="fd-fielderror" role="alert">
              {error}
            </p>
          )}
          <div className="fd-folder-foot">
            <button
              type="button"
              className="fd-primary fd-sm"
              disabled={listing?.path == null}
              onClick={() => {
                if (listing?.path != null) onChange(listing.path);
                setOpen(false);
              }}
            >
              Use this folder
            </button>
            <button type="button" className="fd-ghost fd-sm" onClick={() => setOpen(false)}>
              Close
            </button>
          </div>
        </div>
      ) : null}
    </div>
  );
}
