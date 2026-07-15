import React, { useEffect, useRef, useState } from 'react';
import { fsListHome, reasonOf } from '../api.js';
import { useModal } from '../useModal.js';

// v2.3 — a home-rooted directory picker, opened from the spawn form so you can
// point at a cwd / repos-root / local repo without typing an absolute path.
// It reuses the read-only home explorer API (fsListHome); it only ever RETURNS
// a path (onPick), it never writes. Directories only: the whole point is to
// choose a folder, so files are omitted rather than shown inert.
//
// It opens OVER the spawn form (a modal on a modal), so it owns Esc itself —
// the keydown is stopped here before the board's global handler can close the
// spawn form underneath.

function join(root, rel) {
  if (!rel) return root;
  return `${root.replace(/\/$/, '')}/${rel}`;
}

function Rows({ path, dirs, openSet, picked, depth, onToggle }) {
  const d = dirs[path];
  if (!d) return null;
  if (d.loading && !d.entries) {
    return <div className="fd-dprow dim" style={{ paddingLeft: depth * 14 + 24 }}>loading…</div>;
  }
  if (d.err) {
    return <div className="fd-dprow err" style={{ paddingLeft: depth * 14 + 24 }}>✗ {d.err}</div>;
  }
  const dirsOnly = (d.entries || []).filter((e) => e.type === 'dir');
  if (!dirsOnly.length) {
    return <div className="fd-dprow dim" style={{ paddingLeft: depth * 14 + 24 }}>no sub-folders</div>;
  }
  return (
    <>
      {dirsOnly.map((e) => {
        const p = path ? `${path}/${e.name}` : e.name;
        const isOpen = openSet.has(p);
        return (
          <React.Fragment key={p}>
            <button
              type="button"
              className={`fd-dprow dir${picked === p ? ' picked' : ''}`}
              style={{ paddingLeft: depth * 14 + 6 }}
              onClick={() => onToggle(p)}
              aria-expanded={isOpen}
            >
              <span className="tw">{isOpen ? '▾' : '▸'}</span>
              <span className="ic">🗀</span>
              <span className="nm">{e.name}</span>
            </button>
            {isOpen && (
              <Rows
                path={p}
                dirs={dirs}
                openSet={openSet}
                picked={picked}
                depth={depth + 1}
                onToggle={onToggle}
              />
            )}
          </React.Fragment>
        );
      })}
    </>
  );
}

export default function DirPicker({ root, onPick, onClose }) {
  const [dirs, setDirs] = useState({});
  const [openSet, setOpenSet] = useState(() => new Set(['']));
  const [picked, setPicked] = useState(''); // '' = the home root itself
  const [rootErr, setRootErr] = useState(null);
  const ref = useRef(null);
  useModal(ref);

  const loadDir = async (p) => {
    setDirs((prev) => ({ ...prev, [p]: { ...(prev[p] || {}), loading: true, err: null } }));
    const res = await fsListHome(p);
    if (res.ok && res.json?.ok) {
      if (p === '') setRootErr(null);
      setDirs((prev) => ({ ...prev, [p]: { entries: res.json.entries || [], loading: false, err: null } }));
    } else {
      const reason = reasonOf(res, `list failed (${res.status})`);
      if (p === '') setRootErr(reason);
      setDirs((prev) => ({ ...prev, [p]: { entries: null, loading: false, err: reason } }));
    }
  };

  useEffect(() => { loadDir(''); }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const onToggle = (p) => {
    setPicked(p); // navigating a folder also selects it — the common intent
    const opening = !openSet.has(p);
    setOpenSet((prev) => {
      const next = new Set(prev);
      if (opening) next.add(p); else next.delete(p);
      return next;
    });
    if (opening) loadDir(p);
  };

  const absolute = join(root || '~', picked);

  const onKeyDown = (e) => {
    // own Esc so it doesn't fall through to the board hotkey and close the
    // spawn form underneath; Enter on the tree confirms the current folder
    if (e.key === 'Escape') { e.stopPropagation(); onClose(); }
    else if (e.key === 'Enter') { e.stopPropagation(); e.preventDefault(); onPick(absolute); }
  };

  return (
    <div className="fd-composewrap fd-dpwrap" onClick={onClose}>
      <div
        className="fd-compose fd-dirpicker"
        role="dialog"
        aria-modal="true"
        aria-label="Choose a folder"
        ref={ref}
        onKeyDown={onKeyDown}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="fd-dphead">
          <span className="lbl">🗀 CHOOSE A FOLDER</span>
          <span className="fd-spacer" />
          <button type="button" className="fd-x" aria-label="Close" onClick={onClose}>✕</button>
        </div>
        <div className="sub">Under your home directory. Pick the folder, or type a path outside home in the field.</div>

        {rootErr ? (
          <div className="fd-dperr">✗ {rootErr}</div>
        ) : (
          <div className="fd-dptree">
            <button
              type="button"
              className={`fd-dprow dir root${picked === '' ? ' picked' : ''}`}
              onClick={() => setPicked('')}
            >
              <span className="tw" />
              <span className="ic">🏠</span>
              <span className="nm">~ (home)</span>
            </button>
            <Rows path="" dirs={dirs} openSet={openSet} picked={picked} depth={0} onToggle={onToggle} />
          </div>
        )}

        <div className="fd-dpfoot">
          <span className="sel mono" title={absolute}>{absolute}</span>
          <span className="fd-spacer" />
          <button type="button" className="fd-ghostbtn" onClick={onClose}>Cancel</button>
          <button type="button" className="send" onClick={() => onPick(absolute)}>Use this folder</button>
        </div>
      </div>
    </div>
  );
}
