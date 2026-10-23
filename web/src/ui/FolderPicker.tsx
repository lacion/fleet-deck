// DL-171: the folder of a station: a text input and "Browse…", which opens an inline panel with the
// breadcrumbs of the folder, its sub-folders, the pinned and recent folders, and "Use this folder".
// The spawn form, the Projects form and the Automations form use it.
import { type ReactNode, useEffect, useRef, useState } from 'react';
import { PathTail } from '../chat/PathTail';
import type { DeckSession, WorkspaceListing } from '../gen';
import { readPins, recentDirs, togglePin, writePins } from '../spawn/spawn';
import { homeOf, homeShort, pathParts } from './format';
import { ChevronRight, Folder, FolderGit2, House, Icon, Pin, PinOff } from './icons';

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
  /**
   * The pin button of the path (the pinned folders of the New session sheet). Off in a settings
   * list, where pinning has no meaning next to the value.
   */
  readonly pinnable?: boolean;
  /** A note under the input row (what the field needs): it stays right under the field while the
   * panel is open. */
  readonly hint?: ReactNode;
};

/**
 * The parts of an absolute path with the path up to each part: `/a/b` gives `a`→`/a`, `b`→`/a/b`.
 * A home folder is one `~` crumb (§5 paths: a path under home starts at `~`, never a root-less
 * `home / me`): `/home/me/app` gives `~`→`/home/me`, `app`→`/home/me/app`.
 */
export function crumbsOf(
  path: string,
): readonly { readonly name: string; readonly path: string }[] {
  const home = homeOf(path);
  const root = home ?? '';
  const parts = path
    .slice(root.length)
    .split('/')
    .filter((p) => p !== '');
  const crumbs = parts.map((name, i) => ({
    name,
    path: `${root}/${parts.slice(0, i + 1).join('/')}`,
  }));
  return home === null ? crumbs : [{ name: '~', path: home }, ...crumbs];
}

/** `data-more-below` while folders hide under the bottom edge of the listing (page.css fade). */
function markMore(el: HTMLElement) {
  el.toggleAttribute('data-more-below', el.scrollHeight - el.scrollTop - el.clientHeight > 1);
}

/** The name and the parent of a folder row (§5 paths): the home folder itself is "Home". */
export function folderName(path: string): { readonly parent: string; readonly base: string } {
  const parts = pathParts(path);
  return parts.base === '~' && parts.parent === '' ? { parent: '', base: 'Home' } : parts;
}

/**
 * One pinned or recent folder row (DESIGN-SYSTEM §4.6, §5 paths): a 14px pin, house or folder icon,
 * the name, then the parent in --faint mono. The full path is the title of the row. The New session
 * folder picker and the Files page list their folders with it.
 */
export function FolderRow({
  path,
  pin,
  onPick,
}: {
  readonly path: string;
  readonly pin: boolean;
  readonly onPick: (path: string) => void;
}) {
  const { parent, base } = folderName(path);
  const home = parent === '' && base === 'Home';
  return (
    <li>
      <button
        type="button"
        data-pinned={pin ? '' : undefined}
        title={path}
        aria-label={parent + base}
        onClick={() => onPick(path)}
      >
        <Icon of={pin ? Pin : home ? House : Folder} size={14} />
        <span className="fd-folder-base">{base}</span>
        {/* The parent keeps its end: whole leading folders give way to "…/" (the last folder tells
            the rows apart); the title keeps the full path. */}
        {parent === '' ? null : (
          <span className="fd-folder-parent">
            <PathTail path={parent} />
          </span>
        )}
      </button>
    </li>
  );
}

const message = (e: unknown) => (e instanceof Error ? e.message : 'The station did not answer.');

/**
 * The most pinned and recent folders that the panel shows before "Show all". One more row shows
 * with no toggle: a toggle row that reveals one row is a row of chrome for no gain.
 */
export const QUICK_ROWS = 5;

/** The board preference or the OS asks for less motion (DESIGN-SYSTEM.md §2.5). */
const reducedMotion = (): boolean =>
  document.documentElement.dataset['motion'] === 'reduce' ||
  window.matchMedia?.('(prefers-reduced-motion: reduce)').matches === true;

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
  pinnable = true,
  hint = null,
}: Props) {
  const [open, setOpen] = useState(initialOpen);
  const [listing, setListing] = useState<WorkspaceListing | null>(initialListing);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [pins, setPins] = useState(readPins);
  // The first folder of the allowed roots that was listed: the crumbs above it are not offered.
  const [rootPath, setRootPath] = useState<string | null>(null);
  // "Show all": every pinned and recent folder, not only the first QUICK_ROWS.
  const [allQuick, setAllQuick] = useState(false);
  const listRef = useRef<HTMLUListElement>(null);
  const turn = useRef(0);
  const station = useRef(stationId);
  const root = useRef<HTMLDivElement>(null);
  const browseButton = useRef<HTMLButtonElement>(null);
  // Browse… opened the panel: once its listing (or its error) is in, the panel comes into view.
  const reveal = useRef(false);
  const path = value.trim();
  const pinned = pins[stationId] ?? [];
  const recent = recentDirs(sessions, stationId, pinned);
  const quick = [
    ...pinned.map((p) => ({ p, pin: true })),
    ...recent.map((p) => ({ p, pin: false })),
  ];
  // "Show all" only when it hides two or more rows.
  const quickFolds = quick.length > QUICK_ROWS + 1;
  const quickShown = quickFolds && !allQuick ? quick.slice(0, QUICK_ROWS) : quick;

  // The fade of the listing follows its scroll and each new folder.
  useEffect(() => {
    if (listRef.current) markMore(listRef.current);
  });

  // A folder belongs to its station: another station closes the panel.
  useEffect(() => {
    if (station.current === stationId) return;
    station.current = stationId;
    turn.current += 1;
    setOpen(false);
    setListing(null);
    setRootPath(null);
    setError(null);
    setAllQuick(false);
    reveal.current = false;
  }, [stationId]);

  // A phone opens the panel under the sticky footer of the New session sheet: the field and its
  // panel scroll into view (the scroll margins stop it above the footer and the bottom bar), and
  // the focus moves to the first folder row unless the owner moved it meanwhile.
  useEffect(() => {
    if (!open || !reveal.current || busy || (listing === null && error === null)) return;
    reveal.current = false;
    const at = root.current;
    if (at === null) return;
    at.scrollIntoView?.({ block: 'nearest', behavior: reducedMotion() ? 'auto' : 'smooth' });
    if (document.activeElement === browseButton.current) {
      at.querySelector<HTMLButtonElement>('.fd-folder-list button')?.focus({ preventScroll: true });
    }
  }, [open, busy, listing, error]);

  const show = (next: WorkspaceListing) => {
    setBusy(false);
    setListing(next);
    if (next.path !== null && next.parent === null) setRootPath(next.path);
    if (next.path === null) setRootPath(null);
  };

  // `enterOnly`: a list of the roots with one root opens that root, so that "Use this folder"
  // works at once (the Roots crumb still goes up).
  const browse = (at: string | null, fallback = false, enterOnly = false) => {
    if (list === undefined) return;
    const mine = ++turn.current;
    setBusy(true);
    setError(null);
    list(stationId, at).then(
      (next) => {
        if (mine !== turn.current) return;
        const only =
          enterOnly && next.path === null && next.entries.length === 1
            ? next.entries[0]
            : undefined;
        if (only === undefined) {
          show(next);
          return;
        }
        // The one root opens itself; when it cannot be read, the list of the roots stays, with
        // the root to try again.
        list(stationId, only.path).then(
          (inner) => {
            if (mine === turn.current) show(inner);
          },
          () => {
            if (mine === turn.current) show(next);
          },
        );
      },
      (e: unknown) => {
        if (mine !== turn.current) return;
        setBusy(false);
        if (fallback && at !== null) browse(null, false, enterOnly);
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
    reveal.current = true;
    if (listing === null) browse(path.startsWith('/') ? path : null, path.startsWith('/'), true);
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
    <div className="fd-folder" ref={root}>
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
        {path === '' || !pinnable ? null : (
          <button
            type="button"
            className="fd-iconbtn"
            aria-label={pinned.includes(path) ? 'Unpin this folder' : 'Pin this folder'}
            title={pinned.includes(path) ? 'Unpin this folder' : 'Pin this folder'}
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
            ref={browseButton}
            type="button"
            aria-expanded={open}
            disabled={browseDisabled || stationId === ''}
            onClick={toggle}
          >
            <Icon of={Folder} /> Browse…
          </button>
        )}
      </div>
      {hint}
      {open && list !== undefined ? (
        <div className="fd-folder-panel" role="region" aria-label="Browse the station">
          {quick.length === 0 ? null : (
            <div className="fd-folder-quick fd-recent" aria-label="Recent directories">
              <span className="fd-label">{pinned.length > 0 ? 'Pinned and recent' : 'Recent'}</span>
              {/* A folder row: its name, then its parent from the root (§5 paths); the full path
                  is its title. At most QUICK_ROWS rows and "Show all" while two or more rows
                  hide: the page is the only scroller on a phone. */}
              <ul className="fd-folder-list">
                {quickShown.map(({ p, pin }) => (
                  <FolderRow key={pin ? `pin:${p}` : p} path={p} pin={pin} onPick={pick} />
                ))}
                {quickFolds ? (
                  <li>
                    <button
                      type="button"
                      className="fd-folder-more"
                      onClick={() => setAllQuick(!allQuick)}
                    >
                      {allQuick ? (
                        'Show fewer'
                      ) : (
                        <>
                          Show all<span className="fd-count">{quick.length}</span>
                        </>
                      )}
                    </button>
                  </li>
                ) : null}
              </ul>
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
                        <span
                          aria-current={i === crumbs.length - 1 ? 'location' : undefined}
                          title={c.name === '~' ? c.path : undefined}
                        >
                          {c.name}
                        </span>
                      ) : (
                        <button
                          type="button"
                          className="fd-ghost"
                          title={c.name === '~' ? c.path : undefined}
                          onClick={() => browse(c.path)}
                        >
                          {c.name}
                        </button>
                      )}
                    </li>
                  ))}
                </ol>
              </nav>
              {listing.entries.length === 0 ? (
                <p className="fd-note">
                  {listing.path === null ? 'No folders.' : 'No subfolders.'}
                </p>
              ) : (
                <ul
                  className="fd-folder-list"
                  ref={listRef}
                  onScroll={(event) => markMore(event.currentTarget)}
                >
                  {listing.entries.map((entry) => (
                    <li key={entry.path}>
                      <button
                        type="button"
                        aria-label={`Open ${entry.name}`}
                        onClick={() => browse(entry.path)}
                      >
                        <Icon of={entry.repository ? FolderGit2 : Folder} size={14} />
                        <span className="fd-folder-base">{entry.name}</span>
                        {entry.repository ? <span className="fd-vh">git repository</span> : null}
                        {/* A listing row opens the folder; a Recent row takes it: the chevron
                            tells them apart. */}
                        <Icon of={ChevronRight} size={14} />
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
          {/* The folder that "Use this folder" takes, then the actions at the right: the choice is
              the last action, as Start session is. */}
          <div className="fd-folder-foot">
            {listing?.path == null ? null : (
              <span className="fd-note fd-folder-foot-path" title={listing.path}>
                <bdi>{homeShort(listing.path)}</bdi>
              </span>
            )}
            <button type="button" className="fd-ghost fd-sm" onClick={() => setOpen(false)}>
              Close
            </button>
            <button
              type="button"
              className="fd-sm"
              disabled={listing?.path == null}
              onClick={() => {
                if (listing?.path != null) onChange(listing.path);
                setOpen(false);
              }}
            >
              Use this folder
            </button>
          </div>
        </div>
      ) : null}
    </div>
  );
}
