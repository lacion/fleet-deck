import { Fragment, useEffect, useMemo, useRef, useState } from 'react';
import type * as React from 'react';
import { fsListHome, saveSettings, reasonOf } from '../api.ts';
import { joinBrowsePath, normalizeBrowseRoot, relativeBrowsePath } from '../fsPath.ts';
import { useModal } from '../useModal.ts';
import { basename } from '../util.ts';

// v2.3 — a directory picker, opened from the spawn form so you can point at a
// cwd / repos-root / local repo without typing an absolute path. It reuses the
// read-only browse-root explorer API (fsListHome); it only ever RETURNS a path
// (onPick). Directories only: the whole point is to choose a folder, so files
// are omitted rather than shown inert.
//
// v2.5 — the tree is rooted at the daemon's configurable BROWSE ROOT (D4), not
// hard-wired home; `root` is the resolved absolute path it serves. Two writes
// DO leave here — but only through the daemon's whitelisted settings surface,
// never the fs walls: ☆ pins/unpins a folder in fav_dirs, and "set as default
// root" saves browse_root. Both fail LOUD (the returned reason renders inline,
// no optimistic local mirror — the next snapshot is the source of truth).
//
// It opens OVER the spawn form (a modal on a modal), so it owns Esc itself —
// the keydown is stopped here before the board's global handler can close the
// spawn form underneath.

const FAV_MAX = 20;

// A filesystem entry from fsListHome; only directories are shown.
interface FsEntry {
  name: string;
  type: string;
}

// Per-path load state kept in the `dirs` map.
interface DirState {
  loading?: boolean;
  err?: string | null;
  entries?: FsEntry[] | null;
}

// fsListHome's body, beyond the transport envelope: {ok, entries}.
interface FsListJson {
  ok?: boolean;
  entries?: FsEntry[];
}

interface RowsProps {
  path: string;
  dirs: Record<string, DirState>;
  openSet: Set<string>;
  picked: string;
  depth: number;
  root: string;
  favSet: Set<string>;
  favFull: boolean;
  actionsBusy: boolean;
  onToggle: (p: string) => void;
  onToggleFav: (abs: string, fav: boolean) => void;
}

function Rows({
  path,
  dirs,
  openSet,
  picked,
  depth,
  root,
  favSet,
  favFull,
  actionsBusy,
  onToggle,
  onToggleFav,
}: RowsProps) {
  const d = dirs[path];
  if (!d) return null;
  if (d.loading && !d.entries) {
    return (
      <div className="fd-dprow dim" style={{ paddingLeft: depth * 14 + 24 }}>
        loading…
      </div>
    );
  }
  if (d.err) {
    return (
      <div className="fd-dprow err" style={{ paddingLeft: depth * 14 + 24 }}>
        ✗ {d.err}
      </div>
    );
  }
  const dirsOnly = (d.entries ?? []).filter((e) => e.type === 'dir');
  if (!dirsOnly.length) {
    return (
      <div className="fd-dprow dim" style={{ paddingLeft: depth * 14 + 24 }}>
        no sub-folders
      </div>
    );
  }
  return (
    <>
      {dirsOnly.map((e) => {
        const p = path ? `${path}/${e.name}` : e.name;
        const isOpen = openSet.has(p);
        const abs = joinBrowsePath(root, p);
        const fav = favSet.has(abs);
        const capped = !fav && favFull; // 20-favorite ceiling — can still UNpin
        return (
          <Fragment key={p}>
            {/* the row button and its ☆ are siblings (never a button-in-button)
                in one flex line — the button keeps its depth indent, the star
                sits at the trailing edge */}
            <div className="fd-dprowline">
              <button
                type="button"
                className={`fd-dprow dir${picked === p ? ' picked' : ''}`}
                style={{ paddingLeft: depth * 14 + 6 }}
                onClick={() => {
                  onToggle(p);
                }}
                aria-expanded={isOpen}
              >
                <span className="tw">{isOpen ? '▾' : '▸'}</span>
                <span className="ic">🗀</span>
                <span className="nm">{e.name}</span>
              </button>
              <button
                type="button"
                className={`fd-dpstar${fav ? ' on' : ''}`}
                disabled={capped || actionsBusy}
                aria-pressed={fav}
                title={
                  fav
                    ? 'unpin this favorite'
                    : capped
                      ? `${FAV_MAX} favorites max`
                      : 'pin as a favorite'
                }
                onClick={() => {
                  onToggleFav(abs, fav);
                }}
              >
                {fav ? '★' : '☆'}
              </button>
            </div>
            {isOpen && (
              <Rows
                path={p}
                dirs={dirs}
                openSet={openSet}
                picked={picked}
                depth={depth + 1}
                root={root}
                favSet={favSet}
                favFull={favFull}
                actionsBusy={actionsBusy}
                onToggle={onToggle}
                onToggleFav={onToggleFav}
              />
            )}
          </Fragment>
        );
      })}
    </>
  );
}

interface DirPickerProps {
  root: string;
  favs?: string[];
  onPick: (abs: string) => void;
  onClose: () => void;
}

export default function DirPicker({ root, favs = [], onPick, onClose }: DirPickerProps) {
  const [dirs, setDirs] = useState<Record<string, DirState>>({});
  const [openSet, setOpenSet] = useState(() => new Set(['']));
  const [picked, setPicked] = useState(''); // '' = the browse root itself
  const [rootErr, setRootErr] = useState<string | null>(null);
  const [actionErr, setActionErr] = useState<string | null>(null); // fail-loud ☆ / set-root save error
  const [actionsBusy, setActionsBusy] = useState(false);
  const actionsBusyRef = useRef(false);
  const ref = useRef<HTMLDivElement | null>(null);
  // one generation per root: the [root] effect below bumps it, and any list
  // response captured under an older generation is DROPPED — an in-flight
  // fsListHome from the previous root must never write a stale tree over the
  // reset (same stale-answer doctrine as FileViewer's seq counter).
  const gen = useRef(0);
  // Sequence each relative path independently. Collapse/re-open and favorite
  // reveals may ask for one folder twice while other branches load in parallel;
  // only the newest answer for that folder may publish.
  const loadSeq = useRef(new Map<string, number>());
  useModal(ref);

  // The browse root resolves to an absolute path; '~' is only the pure-fallback
  // label a daemon predating browse_root would leave us with. `normRoot` is the
  // trailing-slash-free form the containment/relativity checks compare against —
  // except the filesystem root, which IS its own trailing slash, so the child
  // prefix is computed once here instead of naively appending '/' (normRoot '/'
  // + '/' would be '//', which matches nothing and disables every chip).
  const normRoot = normalizeBrowseRoot(root);
  const isHomeRoot = !root || root === '~';
  const favSet = useMemo(() => new Set(favs), [favs]);
  const favFull = favs.length >= FAV_MAX;
  // A favorite is reachable from THIS root iff it is the root or nests under it.
  // The daemon never names a root for the browser (containment walls in
  // files.mjs stay put); an out-of-root favorite renders disabled with a hint.
  const favEnabled = (fav: string) => relativeBrowsePath(normRoot, fav) !== null;

  const loadDir = async (p: string) => {
    const my = gen.current;
    const request = (loadSeq.current.get(p) ?? 0) + 1;
    loadSeq.current.set(p, request);
    setDirs((prev) => ({ ...prev, [p]: { ...(prev[p] ?? {}), loading: true, err: null } }));
    const res = await fsListHome(p);
    if (my !== gen.current || loadSeq.current.get(p) !== request) return false;
    if (res.ok && res.json?.ok) {
      if (p === '') setRootErr(null);
      const entries = (res.json as FsListJson).entries ?? [];
      setDirs((prev) => ({ ...prev, [p]: { entries, loading: false, err: null } }));
      return true;
    }
    const reason = reasonOf(res, `list failed (${res.status})`);
    if (p === '') setRootErr(reason);
    setDirs((prev) => ({ ...prev, [p]: { entries: null, loading: false, err: reason } }));
    return false;
  };

  // The tree is bound to `root`; when it changes under us (e.g. "set as default
  // root" round-trips a new browse_root down through the snapshot), throw the
  // whole tree away and reload from the new root — a stale listing from the old
  // root must never linger. Bumping `gen` first invalidates every in-flight
  // response from the old root. On first mount this simply loads '' once.
  useEffect(() => {
    gen.current += 1;
    loadSeq.current.clear();
    setDirs({});
    setOpenSet(new Set(['']));
    setPicked('');
    setRootErr(null);
    void loadDir('');
  }, [root]);

  const onToggle = (p: string) => {
    setPicked(p); // navigating a folder also selects it — the common intent
    const opening = !openSet.has(p);
    setOpenSet((prev) => {
      const next = new Set(prev);
      if (opening) next.add(p);
      else next.delete(p);
      return next;
    });
    if (opening) void loadDir(p);
  };

  // Chip click: reveal the favorite. Open the root and every ancestor prefix
  // down to (and including) the target, lazy-loading each level, then select the
  // target's rel path. Only ever called on a favEnabled chip.
  const jumpToFav = async (fav: string) => {
    const my = gen.current; // the rel path is only meaningful under THIS root
    const rel = relativeBrowsePath(normRoot, fav);
    if (rel === null) return;
    const segs = rel ? rel.split('/') : [];
    const prefixes = [''];
    let cur = '';
    for (const seg of segs) {
      cur = cur ? `${cur}/${seg}` : seg;
      prefixes.push(cur);
    }
    setOpenSet((prev) => {
      const next = new Set(prev);
      for (const pfx of prefixes) next.add(pfx);
      return next;
    });
    for (const pfx of prefixes) {
      if (!dirs[pfx]?.entries) {
        if (!(await loadDir(pfx))) return;
      }
    }
    if (my === gen.current) setPicked(rel);
  };

  // ☆ toggle — flip membership of the ABSOLUTE path in fav_dirs and POST the
  // whole array. No optimistic local flip: on {ok:false} the reason renders
  // inline (fail-loud), and on success the daemon's next snapshot re-supplies
  // `favs` as the source of truth.
  const saveSetting = async (body: unknown) => {
    if (actionsBusyRef.current) return;
    actionsBusyRef.current = true;
    setActionsBusy(true);
    setActionErr(null);
    try {
      const res = await saveSettings(body);
      if (!(res.ok && res.json?.ok)) setActionErr(reasonOf(res, `save failed (${res.status})`));
    } catch {
      setActionErr('save failed — daemon unreachable');
    } finally {
      actionsBusyRef.current = false;
      setActionsBusy(false);
    }
  };

  const toggleFav = async (abs: string, isFav: boolean) => {
    if (actionsBusyRef.current) return;
    if (!isFav && favFull) {
      setActionErr(`${FAV_MAX} favorites max`);
      return;
    }
    const next = isFav ? favs.filter((f) => f !== abs) : [...favs, abs];
    await saveSetting({ fav_dirs: next });
  };

  // "set as default root" — persist browse_root; the new root arrives back as a
  // prop and the [root] effect re-roots the tree. Same fail-loud handling.
  const setDefaultRoot = (abs: string) => saveSetting({ browse_root: abs });

  const absolute = joinBrowsePath(root, picked);
  const pickedIsFav = favSet.has(absolute);
  const pickedCapped = !pickedIsFav && favFull;

  const onKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    // own Esc so it doesn't fall through to the board hotkey and close the
    // spawn form underneath. Confirmation belongs only to the explicit footer
    // button: a dialog-wide Enter handler made Enter on Close, ☆, or a favorite
    // unexpectedly choose the current directory as well.
    if (e.key === 'Escape') {
      e.stopPropagation();
      onClose();
    }
  };

  return (
    <div
      className="fd-composewrap fd-dpwrap"
      onClick={(e) => {
        // This picker is nested inside SpawnForm's backdrop. Own the event so
        // dismissing only the picker never also closes the underlying form.
        e.stopPropagation();
        onClose();
      }}
    >
      <div
        className="fd-compose fd-dirpicker"
        role="dialog"
        aria-modal="true"
        aria-label="Choose a folder"
        ref={ref}
        onKeyDown={onKeyDown}
        onClick={(e) => {
          e.stopPropagation();
        }}
      >
        <div className="fd-dphead">
          <span className="lbl">🗀 CHOOSE A FOLDER</span>
          <span className="fd-spacer" />
          <button type="button" className="fd-x" aria-label="Close" onClick={onClose}>
            ✕
          </button>
        </div>
        <div className="sub">
          Under the browse root below. Pick a folder, or type a path outside it in the form&apos;s
          field. ★ pins a favorite.
        </div>

        {favs.length > 0 && (
          <div className="fd-dpfavs">
            <span className="lbl">favorites</span>
            {favs.map((fav) => {
              const enabled = favEnabled(fav);
              return (
                <button
                  type="button"
                  key={fav}
                  className={`fd-chip fd-dpfav${enabled ? '' : ' off'}`}
                  disabled={!enabled}
                  title={
                    enabled ? fav : 'outside the browse root — set it as default root to reach it'
                  }
                  onClick={
                    enabled
                      ? () => {
                          void jumpToFav(fav);
                        }
                      : undefined
                  }
                >
                  ★ {basename(fav) || fav}
                </button>
              );
            })}
          </div>
        )}

        {rootErr ? (
          <div className="fd-dperr" role="alert">
            ✗ {rootErr}
          </div>
        ) : (
          <div className="fd-dptree">
            <button
              type="button"
              className={`fd-dprow dir root${picked === '' ? ' picked' : ''}`}
              onClick={() => {
                setPicked('');
              }}
            >
              <span className="tw" />
              <span className="ic">{isHomeRoot ? '🏠' : '🗀'}</span>
              <span className="nm">{isHomeRoot ? '~ (home)' : root}</span>
            </button>
            <Rows
              path=""
              dirs={dirs}
              openSet={openSet}
              picked={picked}
              depth={0}
              root={root || '~'}
              favSet={favSet}
              favFull={favFull}
              actionsBusy={actionsBusy}
              onToggle={onToggle}
              onToggleFav={(abs, fav) => {
                void toggleFav(abs, fav);
              }}
            />
          </div>
        )}

        {actionErr && (
          <div className="fd-dpnote err" role="alert">
            ✗ {actionErr}
          </div>
        )}

        <div className="fd-dpfoot">
          <button
            type="button"
            className={`fd-dpstar foot${pickedIsFav ? ' on' : ''}`}
            disabled={pickedCapped || actionsBusy}
            aria-pressed={pickedIsFav}
            title={
              pickedIsFav
                ? 'unpin this favorite'
                : pickedCapped
                  ? `${FAV_MAX} favorites max`
                  : 'pin this folder as a favorite'
            }
            onClick={() => {
              void toggleFav(absolute, pickedIsFav);
            }}
          >
            {pickedIsFav ? '★' : '☆'}
          </button>
          <button
            type="button"
            className="fd-ghostbtn fd-dprootbtn"
            disabled={actionsBusy}
            title={`make ${absolute} the default browse root`}
            onClick={() => {
              void setDefaultRoot(absolute);
            }}
          >
            set as default root
          </button>
          <span className="sel mono" title={absolute}>
            {absolute}
          </span>
          <button type="button" className="fd-ghostbtn" onClick={onClose}>
            Cancel
          </button>
          <button
            type="button"
            className="send"
            onClick={() => {
              onPick(absolute);
            }}
          >
            Use this folder
          </button>
        </div>
      </div>
    </div>
  );
}
