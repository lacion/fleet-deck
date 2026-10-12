import { useState } from 'react';
import type { DeckSession, StationView } from '../gen';
import { FilesPanel } from '../session/FilesPanel';
import type { FileActions } from '../session/workspace';
import { lastParts, readPins } from '../spawn/spawn';
import { Icon, Pin } from '../ui/icons';

/** The most recent session folders that the chip row offers. */
const MAX_FOLDERS = 8;
const ROOT_KEY = 'fd-files-root';

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
): string[] {
  const seen = new Set(pinned);
  const out: string[] = [];
  const mine = sessions
    .filter((s) => s.station_id === station && s.summary.cwd !== null && s.summary.cwd !== '')
    .sort((a, b) => b.summary.updated_at - a.summary.updated_at);
  for (const s of mine) {
    const cwd = s.summary.cwd;
    if (cwd === null || seen.has(cwd)) continue;
    seen.add(cwd);
    out.push(cwd);
    if (out.length === MAX_FOLDERS) break;
  }
  return out;
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
  if (!filesFor) {
    return (
      <section className="fd-screen fd-files-page" aria-label="Files">
        <h1 className="fd-screen-title">Files</h1>
        <p className="fd-note">Files need a live board with the drive scope.</p>
      </section>
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
  const recent = sessionFolders(sessions, station, pinned);
  return (
    <section className="fd-screen fd-files-page" aria-label="Files">
      <h1 className="fd-screen-title">Files</h1>
      <div className="fd-panel fd-files-folder">
        <form
          aria-label="Open a folder"
          onSubmit={(event) => {
            event.preventDefault();
            open(station, folder);
          }}
        >
          <label>
            Station
            <select value={station} onChange={(event) => setStation(event.target.value)}>
              {ordered.map((s) => (
                <option key={s.station_id} value={s.station_id}>
                  {s.name}
                </option>
              ))}
            </select>
          </label>
          <label className="fd-files-folder-path">
            Folder
            <input
              placeholder="/absolute/path/inside/an/allowed/root"
              value={folder}
              onChange={(event) => setFolder(event.target.value)}
            />
          </label>
          <button
            type="submit"
            className="fd-primary fd-sm"
            disabled={folder.trim() === '' || station === ''}
          >
            Open
          </button>
        </form>
        {pinned.length === 0 && recent.length === 0 ? null : (
          <div className="fd-recent" role="group" aria-label="Pinned and recent">
            <span className="fd-label">Pinned and recent</span>
            {pinned.map((path) => (
              <button
                key={`pin:${path}`}
                type="button"
                className="fd-chip"
                data-pinned
                title={path}
                onClick={() => open(station, path)}
              >
                <Icon of={Pin} size={12} />
                {lastParts(path)}
              </button>
            ))}
            {recent.map((path) => (
              <button
                key={path}
                type="button"
                className="fd-chip"
                title={path}
                onClick={() => open(station, path)}
              >
                {lastParts(path)}
              </button>
            ))}
          </div>
        )}
      </div>
      {opened ? (
        <div className="fd-panel fd-files-view">
          <FilesPanel
            key={`${opened.station}\n${opened.folder}`}
            files={filesFor(opened.station, opened.folder)}
            root={opened.folder}
          />
        </div>
      ) : null}
    </section>
  );
}
