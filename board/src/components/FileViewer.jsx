import React, { useEffect, useMemo, useRef, useState } from 'react';
import { fsList, fsRead, fsSearch, reasonOf } from '../api.js';
import { basename, copyText } from '../util.js';
import { renderMarkdown } from '../markdown.js';
import { useModal } from '../useModal.js';

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

const fmtSize = (n) => {
  if (!Number.isFinite(n)) return '';
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
};

const parentOf = (p) => (p.includes('/') ? p.slice(0, p.lastIndexOf('/')) : '');

// Type glyphs: directories carry their own ▸/▾, so files only need to mark the
// two odd cases (a symlink is shown but never followed; 'other' is a FIFO or
// socket the daemon refuses to open — render it inert so nobody keeps clicking).
const GLYPH = { symlink: '⇗', other: '∅' };

function TreeDir({ path, dirs, openSet, curFile, depth, onToggle, onOpenFile }) {
  const d = dirs[path];
  if (!d) return null;
  if (d.loading && !d.entries) {
    return <div className="fd-fsrow dim" style={{ paddingLeft: depth * 14 + 26 }}>loading…</div>;
  }
  if (d.err) {
    return <div className="fd-fsrow err" style={{ paddingLeft: depth * 14 + 26 }}>✗ {d.err}</div>;
  }
  const entries = d.entries || [];
  return (
    <>
      {entries.length === 0 && (
        <div className="fd-fsrow dim" style={{ paddingLeft: depth * 14 + 26 }}>(empty)</div>
      )}
      {entries.map((e) => {
        const p = path ? `${path}/${e.name}` : e.name;
        if (e.type === 'dir') {
          const isOpen = openSet.has(p);
          return (
            <React.Fragment key={p}>
              <button
                type="button"
                className={`fd-fsrow dir${e.ignored ? ' ignored' : ''}`}
                style={{ paddingLeft: depth * 14 + 8 }}
                onClick={() => onToggle(p)}
                aria-expanded={isOpen}
              >
                <span className="tw">{isOpen ? '▾' : '▸'}</span>
                <span className="nm">{e.name}</span>
              </button>
              {isOpen && (
                <TreeDir
                  path={p}
                  dirs={dirs}
                  openSet={openSet}
                  curFile={curFile}
                  depth={depth + 1}
                  onToggle={onToggle}
                  onOpenFile={onOpenFile}
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
            title={openable
              ? `${p} · ${fmtSize(e.size)}${e.ignored ? ' · gitignored' : ''}`
              : (e.type === 'symlink'
                ? `${e.name} is a symlink — the viewer never follows links, so what it points at stays unread`
                : `${e.name} is not a regular file (fifo/socket/device) — nothing here can open it`)}
            onClick={openable ? () => onOpenFile(p) : undefined}
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

function FileBody({ file, targetLine, mdOn }) {
  const hitRef = useRef(null);
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
    return <div className="fd-md fd-fsmd" dangerouslySetInnerHTML={{ __html: renderMarkdown(file.content) }} />;
  }
  const lines = String(file.content ?? '').split('\n');
  const shown = lines.slice(0, MAX_RENDER_LINES);
  const gutter = String(Math.min(lines.length, MAX_RENDER_LINES)).length;
  return (
    <div className="fd-fscode">
      {shown.map((ln, i) => {
        const n = i + 1;
        const hit = n === targetLine;
        return (
          <div key={n} className={`fd-fsline${hit ? ' hit' : ''}`} ref={hit ? hitRef : undefined}>
            <span className="n" style={{ width: `${gutter}ch` }}>{n}</span>
            <span className="c">{ln}</span>
          </div>
        );
      })}
      {lines.length > shown.length && (
        <div className="fd-fsnotice">
          showing the first {MAX_RENDER_LINES.toLocaleString()} lines ({lines.length.toLocaleString()} in the served slice)
        </div>
      )}
    </div>
  );
}

export default function FileViewer({ sid, callsign, root, initialPath, onClose }) {
  const [dirs, setDirs] = useState({});          // relPath -> {entries, truncated, loading, err}
  const [openSet, setOpenSet] = useState(() => new Set(['']));
  const [git, setGit] = useState(null);          // null until the first list answers
  const [rootErr, setRootErr] = useState(null);  // 404/410 — replaces the whole body
  const [file, setFile] = useState(null);        // {path, loading, err, content, size, mtime, binary, truncated}
  const [targetLine, setTargetLine] = useState(null);
  const [mdOn, setMdOn] = useState(false);
  const [q, setQ] = useState('');
  const [mode, setMode] = useState('content');
  const [results, setResults] = useState(null);  // {q, mode, backend, hits, truncated, err, busy}
  const [view, setView] = useState('welcome');   // 'welcome' | 'file' | 'results'

  const ref = useRef(null);
  const searchRef = useRef(null);
  const seq = useRef(0); // one counter for every async surface — stale answers drop
  useModal(ref, { initialFocus: false }); // focus is parked on the search input below

  useEffect(() => { searchRef.current?.focus(); }, []);

  const loadDir = async (p) => {
    setDirs((prev) => ({ ...prev, [p]: { ...(prev[p] || {}), loading: true, err: null } }));
    const res = await fsList(sid, p);
    if (res.ok && res.json?.ok) {
      if (p === '') { setGit(!!res.json.git); setRootErr(null); }
      setDirs((prev) => ({
        ...prev,
        [p]: { entries: res.json.entries || [], truncated: !!res.json.truncated, loading: false, err: null },
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

  const toggleDir = (p) => {
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
    if (opening) loadDir(p);
  };

  const openFile = async (p, line) => {
    const my = ++seq.current;
    setView('file');
    setTargetLine(line ?? null);
    setMdOn(p.endsWith('.md')); // prose renders, one click back to source
    setFile({ path: p, loading: true, err: null, content: '' });
    const res = await fsRead(sid, p);
    if (my !== seq.current) return;
    if (res.ok && res.json?.ok) {
      setFile({ path: p, loading: false, err: null, ...res.json });
    } else {
      setFile({ path: p, loading: false, err: reasonOf(res, `read failed (${res.status})`), content: '' });
    }
  };

  // A path arriving from outside (a FILES chip on the drawer) gets the full
  // treatment: every ancestor directory loads and opens so the tree SHOWS where
  // the file lives, then the file itself opens. Sequential on purpose — each
  // segment's listing is what proves the next segment exists.
  const revealPath = async (p, line) => {
    const segs = p.split('/').filter(Boolean);
    let cur = '';
    for (const seg of segs.slice(0, -1)) {
      cur = cur ? `${cur}/${seg}` : seg;
      const known = dirs[cur]?.entries;
      if (!known) { if (!(await loadDir(cur))) return; }
      setOpenSet((prev) => new Set(prev).add(cur));
    }
    openFile(p, line);
  };

  useEffect(() => {
    (async () => {
      const ok = await loadDir('');
      if (ok && initialPath) revealPath(initialPath);
    })();
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const runSearch = async (query, m) => {
    const my = ++seq.current;
    setResults({ q: query, mode: m, hits: [], busy: true, err: null });
    setView('results');
    const res = await fsSearch(sid, query, m);
    if (my !== seq.current) return;
    if (res.ok && res.json?.ok) {
      setResults({ q: query, mode: m, busy: false, err: null, ...res.json });
    } else {
      setResults({ q: query, mode: m, hits: [], busy: false, err: reasonOf(res, `search failed (${res.status})`) });
    }
  };

  // live search, debounced; under 2 chars the daemon would refuse, so don't ask
  useEffect(() => {
    const query = q.trim();
    if (query.length < 2) {
      setResults(null);
      setView((v) => (v === 'results' ? (file ? 'file' : 'welcome') : v));
      return undefined;
    }
    const t = setTimeout(() => runSearch(query, mode), 300);
    return () => clearTimeout(t);
  }, [q, mode]); // eslint-disable-line react-hooks/exhaustive-deps

  // content hits grouped by file — 40 hits across 6 files should read as 6
  // findable places, not 40 interchangeable rows
  const grouped = useMemo(() => {
    if (!results?.hits?.length) return [];
    const byFile = new Map();
    for (const h of results.hits) {
      if (!byFile.has(h.path)) byFile.set(h.path, []);
      byFile.get(h.path).push(h);
    }
    return [...byFile.entries()];
  }, [results]);

  const onSearchKey = (e) => {
    if (e.key === 'Escape' && q) {
      // first Esc clears the query; only an Esc on an empty box closes the
      // viewer (the board hotkey handles that one)
      e.stopPropagation();
      setQ('');
    } else if (e.key === 'Enter' && q.trim().length >= 2) {
      runSearch(q.trim(), mode);
    }
  };

  const absPath = (p) => (root ? `${root}/${p}` : p);

  return (
    <div className="fd-composewrap" onClick={onClose}>
      <div
        className="fd-compose fd-fsviewer"
        role="dialog"
        aria-modal="true"
        aria-label={`Files — ${callsign}`}
        ref={ref}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="fd-fshead">
          <span className="lbl">⌸ FILES — {callsign}</span>
          <span className="fd-fsroot mono" title={root}>{root}</span>
          {git != null && (
            <span
              className="fd-fsbackend"
              title={git
                ? 'a git working tree — search runs through git and respects .gitignore'
                : 'not a git repository — search scans the directory with hard caps'}
            >
              {git ? '⎇ git' : 'plain dir'}
            </span>
          )}
          <span className="fd-spacer" />
          <button type="button" className="fd-x" aria-label="Close" onClick={onClose}>✕</button>
        </div>

        <div className="fd-fssearch">
          <input
            ref={searchRef}
            className="fd-input"
            placeholder="search file contents… (2+ chars, literal — no regex)"
            value={q}
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={onSearchKey}
          />
          <div className="fd-fsmodes" role="radiogroup" aria-label="Search mode">
            <button
              type="button"
              className={`fd-target${mode === 'content' ? ' on' : ''}`}
              onClick={() => setMode('content')}
            >
              content
            </button>
            <button
              type="button"
              className={`fd-target${mode === 'name' ? ' on' : ''}`}
              onClick={() => setMode('name')}
            >
              file names
            </button>
          </div>
        </div>

        {rootErr ? (
          <div className="fd-fsgone">
            <div className="t1">{rootErr.status === 410 ? 'WORKING TREE GONE' : 'FILES UNAVAILABLE'}</div>
            <div className="t2">
              {rootErr.status === 410
                ? 'The directory this session worked in no longer exists on disk. There is nothing to browse — only the card and its transcript remain.'
                : rootErr.reason}
            </div>
          </div>
        ) : (
          <div className="fd-fsbody">
            <div className="fd-fstree">
              <TreeDir
                path=""
                dirs={dirs}
                openSet={openSet}
                curFile={file?.path || null}
                depth={0}
                onToggle={toggleDir}
                onOpenFile={(p) => openFile(p)}
              />
            </div>

            <div className="fd-fspane">
              {view === 'results' && results && (
                <>
                  <div className="fd-fspanehead">
                    <span className="ttl">
                      {results.busy
                        ? `searching “${results.q}”…`
                        : `${results.hits?.length ?? 0} ${results.mode === 'name' ? 'files' : 'hits'} for “${results.q}”`}
                    </span>
                    {!results.busy && results.truncated && (
                      <span className="fd-fstrunc">first {results.hits.length} only — narrow the search</span>
                    )}
                    {!results.busy && results.backend === 'walk' && git === false && (
                      <span className="fd-fstrunc dim">plain-dir scan, capped</span>
                    )}
                  </div>
                  {results.err && <div className="fd-spawnerr">✗ {results.err}</div>}
                  <div className="fd-fsresults">
                    {!results.busy && !results.err && (results.hits?.length ?? 0) === 0 && (
                      <div className="fd-fsnotice">
                        nothing matches{git ? ' — gitignored files are not searched' : ''}
                      </div>
                    )}
                    {results.mode === 'name'
                      ? (results.hits || []).map((h) => (
                        <button type="button" key={h.path} className="fd-fshit" onClick={() => revealPath(h.path)}>
                          <span className="p">{parentOf(h.path) && <span className="d">{parentOf(h.path)}/</span>}{basename(h.path)}</span>
                        </button>
                      ))
                      : grouped.map(([p, hits]) => (
                        <div key={p} className="fd-fshitgroup">
                          <button type="button" className="fd-fshitfile" onClick={() => revealPath(p)}>
                            {p} <span className="n">{hits.length}</span>
                          </button>
                          {hits.map((h, i) => (
                            <button
                              type="button"
                              key={`${h.line}:${i}`}
                              className="fd-fshit"
                              onClick={() => revealPath(p, h.line)}
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
                      <button type="button" className="fd-ghostbtn fd-fsback" onClick={() => setView('results')}>
                        ← results
                      </button>
                    )}
                    <span className="ttl mono" title={file.path}>{file.path}</span>
                    {!file.loading && !file.err && !file.binary && (
                      <span className="meta">{fmtSize(file.size)}</span>
                    )}
                    {file.path?.endsWith('.md') && !file.binary && (
                      <div className="fd-fsmodes">
                        <button type="button" className={`fd-target${mdOn ? ' on' : ''}`} onClick={() => setMdOn(true)}>rendered</button>
                        <button type="button" className={`fd-target${!mdOn ? ' on' : ''}`} onClick={() => setMdOn(false)}>source</button>
                      </div>
                    )}
                    <span className="fd-spacer" />
                    <button
                      type="button"
                      className="fd-ghostbtn fd-fscopy"
                      title={`copy ${absPath(file.path)}`}
                      onClick={() => copyText(absPath(file.path))}
                    >
                      ⧉ path
                    </button>
                  </div>
                  {file.truncated && !file.binary && (
                    <div className="fd-fstrunc block">
                      ⚠ large file — showing the first {fmtSize(String(file.content ?? '').length)} of {fmtSize(file.size)}
                    </div>
                  )}
                  {file.loading && <div className="fd-fsnotice">reading…</div>}
                  {file.err && <div className="fd-spawnerr">✗ {file.err}</div>}
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
        )}
      </div>
    </div>
  );
}
