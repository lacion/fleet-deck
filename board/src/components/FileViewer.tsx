import React, { useEffect, useMemo, useRef, useState } from 'react';
import { reasonOf, saveSettings } from '../api.ts';
import { basename, copyText } from '../util.ts';
import { renderMarkdown } from '../markdown.ts';
import { useModal } from '../useModal.ts';
import { joinBrowsePath, normalizeBrowseRoot, relativeBrowsePath } from '../fsPath.ts';

// v2.2 — the read-only file viewer: one session's working tree, browsable and
// searchable from the board. Read-only is the contract, not a v1 shortcut —
// the fleet's whole safety story is that the AGENTS edit and the human watches,
// so this surface deliberately has no write affordance to misclick.
//
// Layout: tree on the left (per-directory fetch, cached for the life of the
// modal), content on the right, search across the top. Three honest rules:
//   · the daemon resolves the root from the session id — this component only
//     ever names paths RELATIVE to it, so it cannot ask for /etc/passwd;
//   · every truncation the daemon reports (directory cap, file cap, hit cap)
//     is SHOWN, never smoothed over — a viewer that silently drops the tail of
//     a file is worse than none;
//   · search hits open the file AT the line, and "← results" keeps the hit
//     list one click away — find-read-return is the loop this exists for.

const MAX_RENDER_LINES = 5000; // DOM guard; the daemon's byte cap comes first
const FAV_MAX = 20; // matches the daemon's fav_dirs ceiling

// One filesystem row from a list response; only the fields this surface reads.
interface FsEntry {
  name: string;
  type: string;
  ignored?: boolean;
  size?: number;
}
// A search hit — content mode carries {line, text}, name mode only {path}.
interface Hit {
  path: string;
  line?: number;
  text?: string;
}
// The route-specific JSON bodies the daemon returns (each all-optional, like
// api.ts's ApiJson — the daemon is the shape authority, the browser reads
// defensively). Structurally compatible with ApiResult so reasonOf() applies.
interface FsListJson {
  ok?: boolean;
  git?: boolean;
  entries?: FsEntry[] | null;
  truncated?: boolean;
}
interface FsReadJson {
  ok?: boolean;
  size?: number;
  mtime?: number;
  binary?: boolean;
  truncated?: boolean;
  content?: string;
}
interface FsSearchJson {
  ok?: boolean;
  backend?: string;
  hits?: Hit[];
  truncated?: boolean;
}
interface FsResult<T> {
  ok: boolean;
  status: number;
  json: T | null;
  reason: string | null;
}
type ListResult = FsResult<FsListJson>;
type ReadResult = FsResult<FsReadJson>;
type SearchResult = FsResult<FsSearchJson>;
// Per-directory tree state, keyed by relative path.
interface DirState {
  entries?: FsEntry[] | null;
  truncated?: boolean;
  loading?: boolean;
  err?: string | null;
}
interface RootErr {
  status: number;
  reason: string;
}
// The open file: {path} plus whatever a read response spread over it.
interface FileState {
  path: string;
  loading?: boolean;
  err?: string | null;
  content?: string;
  size?: number;
  mtime?: number;
  binary?: boolean;
  truncated?: boolean;
}
// The search-results panel: {q, mode} plus whatever a search response spread in.
interface ResultsState {
  q: string;
  mode: string;
  hits?: Hit[];
  busy?: boolean;
  err?: string | null;
  truncated?: boolean;
  backend?: string;
}

const fmtSize = (n: number | undefined): string => {
  if (typeof n !== 'number' || !Number.isFinite(n)) return '';
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
};

const parentOf = (p: string): string => (p.includes('/') ? p.slice(0, p.lastIndexOf('/')) : '');

// Type glyphs: directories carry their own ▸/▾, so files only need to mark the
// two odd cases (a symlink is shown but never followed; 'other' is a FIFO or
// socket the daemon refuses to open — render it inert so nobody keeps clicking).
const GLYPH: Record<string, string> = { symlink: '⇗', other: '∅' };

interface TreeDirProps {
  path: string;
  dirs: Record<string, DirState>;
  openSet: Set<string>;
  curFile: string | null;
  depth: number;
  onToggle: (p: string) => void;
  onOpenFile: (p: string) => void;
  favActive?: boolean;
  favSet: Set<string>;
  favFull?: boolean;
  favBusy?: boolean;
  absOf: (p: string) => string;
  onToggleFav: (abs: string | null, isFav: boolean) => void;
}

function TreeDir({
  path,
  dirs,
  openSet,
  curFile,
  depth,
  onToggle,
  onOpenFile,
  favActive = false,
  favSet,
  favFull = false,
  favBusy = false,
  absOf,
  onToggleFav,
}: TreeDirProps) {
  const d = dirs[path];
  if (!d) return null;
  if (d.loading && !d.entries) {
    return (
      <div className="fd-fsrow dim" style={{ paddingLeft: depth * 14 + 26 }}>
        loading…
      </div>
    );
  }
  if (d.err) {
    return (
      <div className="fd-fsrow err" style={{ paddingLeft: depth * 14 + 26 }}>
        ✗ {d.err}
      </div>
    );
  }
  const entries = d.entries ?? [];
  return (
    <>
      {entries.length === 0 && (
        <div className="fd-fsrow dim" style={{ paddingLeft: depth * 14 + 26 }}>
          (empty)
        </div>
      )}
      {entries.map((e) => {
        const p = path ? `${path}/${e.name}` : e.name;
        if (e.type === 'dir') {
          const isOpen = openSet.has(p);
          // favorites are OPT-IN: without the props this renders exactly as it
          // always has (a bare dir button). With them, the ☆ rides as a sibling
          // in a flex line — never a button-in-button.
          const abs = favActive ? absOf(p) : null;
          const fav = favActive && favSet.has(abs ?? '');
          const capped = favActive && !fav && favFull; // 20-favorite ceiling
          const dirBtn = (
            <button
              type="button"
              className={`fd-fsrow dir${e.ignored ? ' ignored' : ''}`}
              style={{ paddingLeft: depth * 14 + 8 }}
              onClick={() => {
                onToggle(p);
              }}
              aria-expanded={isOpen}
            >
              <span className="tw">{isOpen ? '▾' : '▸'}</span>
              <span className="nm">{e.name}</span>
            </button>
          );
          return (
            <React.Fragment key={p}>
              {favActive ? (
                <div className="fd-fsrowline">
                  {dirBtn}
                  <button
                    type="button"
                    className={`fd-fsstar${fav ? ' on' : ''}`}
                    disabled={capped || favBusy}
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
              ) : (
                dirBtn
              )}
              {isOpen && (
                <TreeDir
                  path={p}
                  dirs={dirs}
                  openSet={openSet}
                  curFile={curFile}
                  depth={depth + 1}
                  onToggle={onToggle}
                  onOpenFile={onOpenFile}
                  favActive={favActive}
                  favSet={favSet}
                  favFull={favFull}
                  favBusy={favBusy}
                  absOf={absOf}
                  onToggleFav={onToggleFav}
                />
              )}
            </React.Fragment>
          );
        }
        const openable = e.type === 'file';
        return (
          <button
            key={p}
            type="button"
            className={`fd-fsrow file${e.ignored ? ' ignored' : ''}${curFile === p ? ' cur' : ''}${openable ? '' : ' inert'}`}
            style={{ paddingLeft: depth * 14 + 26 }}
            disabled={!openable}
            title={
              openable
                ? `${p} · ${fmtSize(e.size)}${e.ignored ? ' · gitignored' : ''}`
                : e.type === 'symlink'
                  ? `${e.name} is a symlink — the viewer never follows links, so what it points at stays unread`
                  : `${e.name} is not a regular file (fifo/socket/device) — nothing here can open it`
            }
            onClick={
              openable
                ? () => {
                    onOpenFile(p);
                  }
                : undefined
            }
          >
            {GLYPH[e.type] && <span className="tw">{GLYPH[e.type]}</span>}
            <span className="nm">{e.name}</span>
          </button>
        );
      })}
      {d.truncated && (
        <div className="fd-fsrow warn" style={{ paddingLeft: depth * 14 + 26 }}>
          ⚠ long directory — showing the first {entries.length} entries
        </div>
      )}
    </>
  );
}

interface FileBodyProps {
  file: FileState;
  targetLine: number | null;
  mdOn: boolean;
}

function FileBody({ file, targetLine, mdOn }: FileBodyProps) {
  const hitRef = useRef<HTMLDivElement | null>(null);
  // center the hit once per open — file.path in deps, not the ref, so scrolling
  // around afterwards isn't yanked back on unrelated re-renders
  useEffect(() => {
    hitRef.current?.scrollIntoView({ block: 'center' });
  }, [file.path, targetLine]);

  if (file.binary) {
    return (
      <div className="fd-fsnotice">
        binary file · {fmtSize(file.size)} — nothing sensible to render
      </div>
    );
  }
  if (mdOn) {
    // renderMarkdown HTML-escapes everything before adding a single tag — the
    // module's stated contract — so untrusted file content cannot inject markup.
    return (
      <div
        className="fd-md fd-fsmd"
        dangerouslySetInnerHTML={{ __html: renderMarkdown(file.content) }}
      />
    );
  }
  const lines = (file.content ?? '').split('\n');
  const shown = lines.slice(0, MAX_RENDER_LINES);
  const gutter = String(Math.min(lines.length, MAX_RENDER_LINES)).length;
  return (
    <div className="fd-fscode">
      {shown.map((ln, i) => {
        const n = i + 1;
        const hit = n === targetLine;
        return (
          <div key={n} className={`fd-fsline${hit ? ' hit' : ''}`} ref={hit ? hitRef : undefined}>
            <span className="n" style={{ width: `${gutter}ch` }}>
              {n}
            </span>
            <span className="c">{ln}</span>
          </div>
        );
      })}
      {lines.length > shown.length && (
        <div className="fd-fsnotice">
          showing the first {MAX_RENDER_LINES.toLocaleString()} lines (
          {lines.length.toLocaleString()} in the served slice)
        </div>
      )}
    </div>
  );
}

interface FileViewerProps {
  list: (p: string) => Promise<ListResult>;
  read: (p: string) => Promise<ReadResult>;
  search: (query: string, m: 'content' | 'name') => Promise<SearchResult>;
  title: string;
  root: string;
  initialPath?: string | null;
  favs?: string[];
  onClose: () => void;
}

// list/read/search are already bound to a scope (a session's working tree, or
// the global home root) by the parent — this component doesn't know or care
// which, so the same tree/search/read UI serves both. title labels the header.
// v2.5 — `favs` is OPTIONAL. When it is provided (the GLOBAL home explorer), a
// favorites chip strip + ☆ affordances on directory rows appear, wired through
// the daemon's whitelisted settings surface (fav_dirs). When it is absent (the
// session-scoped instance), every favorites branch short-circuits and the
// viewer renders byte-for-byte as it did before this prop existed.
export default function FileViewer({
  list,
  read,
  search,
  title,
  root,
  initialPath,
  favs,
  onClose,
}: FileViewerProps) {
  const [dirs, setDirs] = useState<Record<string, DirState>>({}); // relPath -> {entries, truncated, loading, err}
  const [openSet, setOpenSet] = useState<Set<string>>(() => new Set(['']));
  const [git, setGit] = useState<boolean | null>(null); // null until the first list answers
  const [rootErr, setRootErr] = useState<RootErr | null>(null); // 404/410 — replaces the whole body
  const [file, setFile] = useState<FileState | null>(null); // {path, loading, err, content, size, mtime, binary, truncated}
  const [targetLine, setTargetLine] = useState<number | null>(null);
  const [mdOn, setMdOn] = useState(false);
  const [q, setQ] = useState('');
  const [mode, setMode] = useState<'content' | 'name'>('content');
  const [results, setResults] = useState<ResultsState | null>(null); // {q, mode, backend, hits, truncated, err, busy}
  const [view, setView] = useState('welcome'); // 'welcome' | 'file' | 'results'
  const [favErr, setFavErr] = useState<string | null>(null); // fail-loud ☆ save error (favActive only)
  const [favBusy, setFavBusy] = useState(false);

  // favorites: opt-in, and only ever touched when favs is an array
  const favActive = Array.isArray(favs);
  // trailing-slash-free root for the containment math — except the filesystem
  // root, which IS its own trailing slash, so the child prefix is computed once
  // instead of naively appending '/' ('/' + '/' would be '//', matching nothing
  // and disabling every chip)
  const normRoot = normalizeBrowseRoot(root);
  const favSet = useMemo(() => new Set(favActive ? favs : []), [favs, favActive]);
  const favFull = favActive && favs.length >= FAV_MAX;
  // reachable from THIS root iff it is the root or nests under it — the browser
  // never names a root; out-of-root favorites render disabled with a hint
  const favEnabled = (fav: string) => relativeBrowsePath(normRoot, fav) !== null;

  const ref = useRef<HTMLDivElement | null>(null);
  const searchRef = useRef<HTMLInputElement | null>(null);
  // Reads, searches, and directory branches are independent async surfaces.
  // Giving each its own sequence prevents a search from cancelling a file read
  // (or vice versa), while still dropping stale answers within that surface.
  const fileSeq = useRef(0);
  const searchSeq = useRef(0);
  const dirSeq = useRef(new Map<string, number>());
  const searchTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const favBusyRef = useRef(false);
  useModal(ref, { initialFocus: false }); // focus is parked on the search input below

  useEffect(() => {
    searchRef.current?.focus();
  }, []);

  const loadDir = async (p: string): Promise<boolean> => {
    const request = (dirSeq.current.get(p) ?? 0) + 1;
    dirSeq.current.set(p, request);
    setDirs((prev) => ({ ...prev, [p]: { ...(prev[p] ?? {}), loading: true, err: null } }));
    const res = await list(p);
    if (dirSeq.current.get(p) !== request) return false;
    if (res.ok && res.json?.ok) {
      if (p === '') {
        setGit(!!res.json.git);
        setRootErr(null);
      }
      setDirs((prev) => ({
        ...prev,
        [p]: {
          entries: res.json?.entries ?? [],
          truncated: !!res.json?.truncated,
          loading: false,
          err: null,
        },
      }));
      return true;
    }
    const reason = reasonOf(res, `list failed (${res.status})`);
    // the root failing 404/410 means the viewer has nothing to stand on —
    // subdirectories failing is local news, shown on the row
    if (p === '') setRootErr({ status: res.status, reason });
    setDirs((prev) => ({ ...prev, [p]: { entries: null, loading: false, err: reason } }));
    return false;
  };

  const toggleDir = (p: string) => {
    const opening = !openSet.has(p);
    setOpenSet((prev) => {
      const next = new Set(prev);
      if (opening) next.add(p);
      else next.delete(p);
      return next;
    });
    // refetch on EVERY expand, not just the first: the agents are writing while
    // you watch, and a listing cached at open-time quietly lies within minutes.
    // Stale entries stay painted while the refresh runs (no flicker).
    if (opening) void loadDir(p);
  };

  const openFile = async (p: string, line?: number) => {
    const my = ++fileSeq.current;
    setView('file');
    setTargetLine(line ?? null);
    setMdOn(p.endsWith('.md')); // prose renders, one click back to source
    setFile({ path: p, loading: true, err: null, content: '' });
    const res = await read(p);
    if (my !== fileSeq.current) return;
    if (res.ok && res.json?.ok) {
      setFile({ path: p, loading: false, err: null, ...res.json });
    } else {
      setFile({
        path: p,
        loading: false,
        err: reasonOf(res, `read failed (${res.status})`),
        content: '',
      });
    }
  };

  // A path arriving from outside (a FILES chip on the drawer) gets the full
  // treatment: every ancestor directory loads and opens so the tree SHOWS where
  // the file lives, then the file itself opens. Sequential on purpose — each
  // segment's listing is what proves the next segment exists.
  const revealPath = async (p: string, line?: number) => {
    const segs = p.split('/').filter(Boolean);
    let cur = '';
    for (const seg of segs.slice(0, -1)) {
      cur = cur ? `${cur}/${seg}` : seg;
      const known = dirs[cur]?.entries;
      if (!known) {
        if (!(await loadDir(cur))) return;
      }
      setOpenSet((prev) => new Set(prev).add(cur));
    }
    void openFile(p, line);
  };

  // v2.5 — chip click: reveal a favorite directory by opening the root and every
  // ancestor prefix down to it, lazy-loading each level (like revealPath, but the
  // target is a folder, so no file opens). Only ever called on a favEnabled chip.
  const revealDir = async (fav: string) => {
    const rel = relativeBrowsePath(normRoot, fav);
    if (rel === null) return;
    const segs = rel ? rel.split('/') : [];
    let cur = '';
    for (const seg of segs) {
      cur = cur ? `${cur}/${seg}` : seg;
      if (!dirs[cur]?.entries) {
        if (!(await loadDir(cur))) return;
      }
      setOpenSet((prev) => new Set(prev).add(cur));
    }
  };

  // v2.5 — ☆ toggle: flip membership of the ABSOLUTE path in fav_dirs and POST
  // the whole array. No optimistic flip — on {ok:false} the reason renders
  // inline (fail-loud), and success re-supplies `favs` via the next snapshot.
  const toggleFav = async (abs: string | null, isFav: boolean) => {
    if (favBusyRef.current) return;
    if (!isFav && favFull) {
      setFavErr(`${FAV_MAX} favorites max`);
      return;
    }
    favBusyRef.current = true;
    setFavBusy(true);
    setFavErr(null);
    try {
      const next = isFav ? (favs ?? []).filter((f) => f !== abs) : [...(favs ?? []), abs];
      const res = await saveSettings({ fav_dirs: next });
      if (!(res.ok && res.json?.ok)) setFavErr(reasonOf(res, `save failed (${res.status})`));
    } catch {
      setFavErr('save failed — daemon unreachable');
    } finally {
      favBusyRef.current = false;
      setFavBusy(false);
    }
  };

  useEffect(() => {
    void (async () => {
      const ok = await loadDir('');
      if (ok && initialPath) void revealPath(initialPath);
    })();
  }, []);

  const runSearch = async (query: string, m: 'content' | 'name') => {
    const my = ++searchSeq.current;
    setResults({ q: query, mode: m, hits: [], busy: true, err: null });
    setView('results');
    const res = await search(query, m);
    if (my !== searchSeq.current) return;
    if (res.ok && res.json?.ok) {
      setResults({ q: query, mode: m, busy: false, err: null, ...res.json });
    } else {
      setResults({
        q: query,
        mode: m,
        hits: [],
        busy: false,
        err: reasonOf(res, `search failed (${res.status})`),
      });
    }
  };

  // live search, debounced; under 2 chars the daemon would refuse, so don't ask
  useEffect(() => {
    const query = q.trim();
    if (query.length < 2) {
      searchSeq.current += 1; // invalidate a request already on the wire
      clearTimeout(searchTimer.current ?? undefined);
      searchTimer.current = null;
      setResults(null);
      setView((v) => (v === 'results' ? (file ? 'file' : 'welcome') : v));
      return undefined;
    }
    searchTimer.current = setTimeout(() => void runSearch(query, mode), 300);
    return () => {
      clearTimeout(searchTimer.current ?? undefined);
      searchTimer.current = null;
    };
  }, [q, mode]);

  // content hits grouped by file — 40 hits across 6 files should read as 6
  // findable places, not 40 interchangeable rows
  const grouped = useMemo(() => {
    if (!results?.hits?.length) return [];
    const byFile = new Map<string, Hit[]>();
    for (const h of results.hits) {
      if (!byFile.has(h.path)) byFile.set(h.path, []);
      byFile.get(h.path)?.push(h);
    }
    return [...byFile.entries()];
  }, [results]);

  const onSearchKey = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Escape' && q) {
      // first Esc clears the query; only an Esc on an empty box closes the
      // viewer (the board hotkey handles that one)
      e.stopPropagation();
      setQ('');
    } else if (e.key === 'Enter' && q.trim().length >= 2) {
      clearTimeout(searchTimer.current ?? undefined);
      searchTimer.current = null;
      void runSearch(q.trim(), mode);
    }
  };

  const absPath = (p: string) => joinBrowsePath(normRoot, p);

  return (
    <div className="fd-composewrap" onClick={onClose}>
      <div
        className="fd-compose fd-fsviewer"
        role="dialog"
        aria-modal="true"
        aria-label={`Files — ${title}`}
        ref={ref}
        onClick={(e) => {
          e.stopPropagation();
        }}
      >
        <div className="fd-fshead">
          <span className="lbl">⌸ FILES — {title}</span>
          <span className="fd-fsroot mono" title={root}>
            {root}
          </span>
          {git != null && (
            <span
              className="fd-fsbackend"
              title={
                git
                  ? 'a git working tree — search runs through git and respects .gitignore'
                  : 'not a git repository — search scans the directory with hard caps'
              }
            >
              {git ? '⎇ git' : 'plain dir'}
            </span>
          )}
          <span className="fd-spacer" />
          <button type="button" className="fd-x" aria-label="Close" onClick={onClose}>
            ✕
          </button>
        </div>

        <div className="fd-fssearch">
          <input
            ref={searchRef}
            className="fd-input"
            aria-label="Search files"
            placeholder="search file contents… (2+ chars, literal — no regex)"
            value={q}
            onChange={(e) => {
              setQ(e.target.value);
            }}
            onKeyDown={onSearchKey}
          />
          <div className="fd-fsmodes" role="radiogroup" aria-label="Search mode">
            <button
              type="button"
              role="radio"
              aria-checked={mode === 'content'}
              className={`fd-target${mode === 'content' ? ' on' : ''}`}
              onClick={() => {
                setMode('content');
              }}
            >
              content
            </button>
            <button
              type="button"
              role="radio"
              aria-checked={mode === 'name'}
              className={`fd-target${mode === 'name' ? ' on' : ''}`}
              onClick={() => {
                setMode('name');
              }}
            >
              file names
            </button>
          </div>
        </div>

        {rootErr ? (
          <div className="fd-fsgone" role="alert">
            <div className="t1">
              {rootErr.status === 410 ? 'WORKING TREE GONE' : 'FILES UNAVAILABLE'}
            </div>
            <div className="t2">
              {rootErr.status === 410
                ? 'The directory this session worked in no longer exists on disk. There is nothing to browse — only the card and its transcript remain.'
                : rootErr.reason}
            </div>
          </div>
        ) : (
          <>
            {/* v2.5 — favorites chip strip (global explorer only). Enabled iff the
                favorite nests under this root; out-of-root ones sit disabled with
                a hint, never naming a root the browser may not reach. */}
            {favActive && favs.length > 0 && (
              <div className="fd-fsfavs">
                <span className="lbl">favorites</span>
                {favs.map((fav) => {
                  const enabled = favEnabled(fav);
                  return (
                    <button
                      type="button"
                      key={fav}
                      className={`fd-chip fd-fsfav${enabled ? '' : ' off'}`}
                      disabled={!enabled}
                      title={
                        enabled
                          ? fav
                          : 'outside the browse root — set it as default root to reach it'
                      }
                      onClick={enabled ? () => void revealDir(fav) : undefined}
                    >
                      ★ {basename(fav)}
                    </button>
                  );
                })}
              </div>
            )}
            {favActive && favErr && (
              <div className="fd-fsfav-err" role="alert">
                ✗ {favErr}
              </div>
            )}
            <div className="fd-fsbody">
              <div className="fd-fstree">
                <TreeDir
                  path=""
                  dirs={dirs}
                  openSet={openSet}
                  curFile={file?.path ?? null}
                  depth={0}
                  onToggle={toggleDir}
                  onOpenFile={(p) => void openFile(p)}
                  favActive={favActive}
                  favSet={favSet}
                  favFull={favFull}
                  favBusy={favBusy}
                  absOf={absPath}
                  onToggleFav={(abs, isFav) => void toggleFav(abs, isFav)}
                />
              </div>

              <div className="fd-fspane">
                {view === 'results' && results && (
                  <>
                    <div className="fd-fspanehead" role="status" aria-live="polite">
                      <span className="ttl">
                        {results.busy
                          ? `searching “${results.q}”…`
                          : `${results.hits?.length ?? 0} ${results.mode === 'name' ? 'files' : 'hits'} for “${results.q}”`}
                      </span>
                      {!results.busy && results.truncated && (
                        <span className="fd-fstrunc">
                          first {results.hits?.length ?? 0} only — narrow the search
                        </span>
                      )}
                      {!results.busy && results.backend === 'walk' && git === false && (
                        <span className="fd-fstrunc dim">plain-dir scan, capped</span>
                      )}
                    </div>
                    {results.err && (
                      <div className="fd-spawnerr" role="alert">
                        ✗ {results.err}
                      </div>
                    )}
                    <div className="fd-fsresults">
                      {!results.busy && !results.err && (results.hits?.length ?? 0) === 0 && (
                        <div className="fd-fsnotice">
                          nothing matches{git ? ' — gitignored files are not searched' : ''}
                        </div>
                      )}
                      {results.mode === 'name'
                        ? (results.hits ?? []).map((h) => (
                            <button
                              type="button"
                              key={h.path}
                              className="fd-fshit"
                              onClick={() => void revealPath(h.path)}
                            >
                              <span className="p">
                                {parentOf(h.path) && <span className="d">{parentOf(h.path)}/</span>}
                                {basename(h.path)}
                              </span>
                            </button>
                          ))
                        : grouped.map(([p, hits]) => (
                            <div key={p} className="fd-fshitgroup">
                              <button
                                type="button"
                                className="fd-fshitfile"
                                onClick={() => void revealPath(p)}
                              >
                                {p} <span className="n">{hits.length}</span>
                              </button>
                              {hits.map((h, i) => (
                                <button
                                  type="button"
                                  key={`${h.line ?? ''}:${i}`}
                                  className="fd-fshit"
                                  onClick={() => void revealPath(p, h.line)}
                                >
                                  <span className="ln">{h.line}</span>
                                  <span className="tx">{h.text}</span>
                                </button>
                              ))}
                            </div>
                          ))}
                    </div>
                  </>
                )}

                {view === 'file' && file && (
                  <>
                    <div className="fd-fspanehead">
                      {results && (
                        <button
                          type="button"
                          className="fd-ghostbtn fd-fsback"
                          onClick={() => {
                            setView('results');
                          }}
                        >
                          ← results
                        </button>
                      )}
                      <span className="ttl mono" title={file.path}>
                        {file.path}
                      </span>
                      {!file.loading && !file.err && !file.binary && (
                        <span className="meta">{fmtSize(file.size)}</span>
                      )}
                      {file.path.endsWith('.md') && !file.binary && (
                        <div className="fd-fsmodes" role="radiogroup" aria-label="Markdown view">
                          <button
                            type="button"
                            role="radio"
                            aria-checked={mdOn}
                            className={`fd-target${mdOn ? ' on' : ''}`}
                            onClick={() => {
                              setMdOn(true);
                            }}
                          >
                            rendered
                          </button>
                          <button
                            type="button"
                            role="radio"
                            aria-checked={!mdOn}
                            className={`fd-target${!mdOn ? ' on' : ''}`}
                            onClick={() => {
                              setMdOn(false);
                            }}
                          >
                            source
                          </button>
                        </div>
                      )}
                      <span className="fd-spacer" />
                      <button
                        type="button"
                        className="fd-ghostbtn fd-fscopy"
                        title={`copy ${absPath(file.path)}`}
                        onClick={() => void copyText(absPath(file.path))}
                      >
                        ⧉ path
                      </button>
                    </div>
                    {file.truncated && !file.binary && (
                      <div className="fd-fstrunc block">
                        ⚠ large file — showing the first {fmtSize((file.content ?? '').length)} of{' '}
                        {fmtSize(file.size)}
                      </div>
                    )}
                    {file.loading && (
                      <div className="fd-fsnotice" role="status">
                        reading…
                      </div>
                    )}
                    {file.err && (
                      <div className="fd-spawnerr" role="alert">
                        ✗ {file.err}
                      </div>
                    )}
                    {!file.loading && !file.err && (
                      <FileBody file={file} targetLine={targetLine} mdOn={mdOn} />
                    )}
                  </>
                )}

                {view === 'welcome' && (
                  <div className="fd-fsgone">
                    <div className="t1">READ-ONLY</div>
                    <div className="t2">
                      Click a file in the tree, or search — content search finds the string,
                      file-name search finds the file. Nothing in here can modify the tree.
                    </div>
                  </div>
                )}
              </div>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
