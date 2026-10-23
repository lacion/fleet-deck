import { useId, useState } from 'react';
import type { DeckSession, StationView } from '../gen';
import { FilesPanel } from '../session/FilesPanel';
import type { FileActions } from '../session/workspace';
import { readPins } from '../spawn/spawn';
import { FolderRow } from '../ui/FolderPicker';
import { FolderOpen } from '../ui/icons';
import { EmptyState, Field, Page } from '../ui/page';

/** The most folder rows that the page offers: every pinned folder, then the recent session
 * folders up to this number. */
const MAX_ROWS = 6;
const ROOT_KEY = 'fd-files-root';
/** The summary sentence under the title (DESIGN-SYSTEM §4.3). */
const FILES_DESC = 'Browse and open the files of a station.';

type Opened = { readonly station: string; readonly folder: string };

/** Linked stations first; the order of the board otherwise. */
function linkedFirst(stations: readonly StationView[]): StationView[] {
  return [...stations.filter((s) => s.reachable), ...stations.filter((s) => !s.reachable)];
}

function readOpened(stations: readonly StationView[]): Opened | null {
  try {
    const raw: unknown = JSON.parse(localStorage.getItem(ROOT_KEY) ?? 'null');
    if (typeof raw !== 'object' || raw === null) return null;
    const { station, folder } = raw as Record<string, unknown>;
    if (typeof station !== 'string' || typeof folder !== 'string' || folder === '') return null;
    return stations.some((s) => s.station_id === station) ? { station, folder } : null;
  } catch {
    return null;
  }
}

function writeOpened(opened: Opened): void {
  try {
    localStorage.setItem(ROOT_KEY, JSON.stringify(opened));
  } catch {
    // Storage is blocked or full: the folder is not kept.
  }
}

/** The `cwd` of the station's sessions, newest first, each once, without the pinned ones. */
function sessionFolders(
  sessions: readonly DeckSession[],
  station: string,
  pinned: readonly string[],
  max: number,
): string[] {
  const seen = new Set(pinned);
  const out: string[] = [];
  if (max <= 0) return out;
  const mine = sessions
    .filter((s) => s.station_id === station && s.summary.cwd !== null && s.summary.cwd !== '')
    .sort((a, b) => b.summary.updated_at - a.summary.updated_at);
  for (const s of mine) {
    const cwd = s.summary.cwd;
    if (cwd === null || seen.has(cwd)) continue;
    seen.add(cwd);
    out.push(cwd);
    if (out.length === max) break;
  }
  return out;
}

/**
 * The pinned and recent folders as folder rows, the look of the New session folder picker
 * (`FolderRow`, DESIGN-SYSTEM §4.6, §5 paths).
 */
export function FolderRows({
  pinned,
  recent,
  onPick,
}: {
  readonly pinned: readonly string[];
  readonly recent: readonly string[];
  readonly onPick: (path: string) => void;
}) {
  return (
    <ul className="fd-folder-list fd-files-folders">
      {[...pinned.map((p) => ({ p, pin: true })), ...recent.map((p) => ({ p, pin: false }))].map(
        ({ p, pin }) => (
          <FolderRow key={pin ? `pin:${p}` : p} path={p} pin={pin} onPick={onPick} />
        ),
      )}
    </ul>
  );
}

/** The global Files page (DL-141): browse and read the files of any folder of a station. */
export function FilesPage({
  stations,
  sessions,
  filesFor,
}: {
  readonly stations: readonly StationView[];
  readonly sessions: readonly DeckSession[];
  readonly filesFor?: ((station: string, workspace: string) => FileActions) | undefined;
}) {
  const ordered = linkedFirst(stations);
  const [opened, setOpened] = useState<Opened | null>(() => readOpened(stations));
  const [station, setStation] = useState(opened?.station ?? ordered[0]?.station_id ?? '');
  const [folder, setFolder] = useState(opened?.folder ?? '');
  const ids = useId();
  if (!filesFor) {
    return (
      <Page title="Files" description={FILES_DESC} className="fd-files-page">
        <p className="fd-note">Files need a live board with the drive scope.</p>
      </Page>
    );
  }
  const open = (at: string, path: string) => {
    const target = path.trim();
    if (target === '' || at === '') return;
    const next = { station: at, folder: target };
    setFolder(target);
    setOpened(next);
    writeOpened(next);
  };
  const pinned = readPins()[station] ?? [];
  const recent = sessionFolders(sessions, station, pinned, MAX_ROWS - pinned.length);
  const here = stations.find((s) => s.station_id === station)?.name ?? 'the station';
  // "Pinned and recent" only when a folder is pinned (the folder picker's words).
  const rowsLabel = pinned.length > 0 ? 'Pinned and recent' : 'Recent';
  return (
    <Page title="Files" description={FILES_DESC} className="fd-files-page" wide>
      {/* The form of a settings page (§4.7): `Field layout="row"`, a 200px label column and the
          controls after it, on the canvas (no boxed toolbar). */}
      <form
        className="fd-files-folder"
        aria-label="Open a folder"
        onSubmit={(event) => {
          event.preventDefault();
          open(station, folder);
        }}
      >
        <Field label="Station" layout="row">
          <select value={station} onChange={(event) => setStation(event.target.value)}>
            {ordered.map((s) => (
              <option key={s.station_id} value={s.station_id}>
                {s.name}
              </option>
            ))}
          </select>
        </Field>
        {/* The markup of a row Field with its own label: the label names the input only, not
            the Open button next to it. */}
        <div className="fd-fieldwrap" data-layout="row">
          <label className="fd-fieldlabel" htmlFor={`${ids}-folder`}>
            Folder
          </label>
          <div className="fd-files-folder-path">
            <input
              id={`${ids}-folder`}
              placeholder={`Folder path on ${here}`}
              value={folder}
              onChange={(event) => setFolder(event.target.value)}
            />
            <button
              type="submit"
              className="fd-primary"
              disabled={folder.trim() === '' || station === ''}
            >
              Open
            </button>
          </div>
        </div>
        {pinned.length === 0 && recent.length === 0 ? null : (
          <Field label={rowsLabel} layout="row" group>
            <FolderRows pinned={pinned} recent={recent} onPick={(p) => open(station, p)} />
          </Field>
        )}
      </form>
      {opened ? (
        <div className="fd-panel fd-files-view">
          <FilesPanel
            key={`${opened.station}\n${opened.folder}`}
            files={filesFor(opened.station, opened.folder)}
            root={opened.folder}
          />
        </div>
      ) : (
        // What comes next (§4.7): Open is the action, so the empty state has no button. It starts
        // on the control column, under the form it explains.
        <EmptyState
          icon={FolderOpen}
          title="Open a folder"
          body={
            pinned.length === 0 && recent.length === 0
              ? 'Type the path of a folder on this station, then press Open.'
              : 'Pick a recent folder above, or type the path of a folder on this station.'
          }
        />
      )}
    </Page>
  );
}
