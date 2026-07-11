import React, { useMemo, useState } from 'react';
import { human, copyText } from '../util.js';

// v1.9 — the worktrees a spawn left behind.
//
// A worktree is a real checkout on disk with a real branch. When the agent that
// owned it dies the directory stays, and until now the only trace of it was a
// list of paths in the Clear toast. This modal is where you SEE them and decide.
//
// The one rule this whole component exists to enforce: removing a worktree that
// holds uncommitted changes or unpushed commits destroys them permanently, and
// nothing on this board should let that happen by accident. So:
//   · the daemon judges (verdict + evidence); the board NEVER guesses.
//   · 'safe' means nothing would be lost — Remove just works, no ceremony.
//   · 'has-work' and 'unknown' go through a confirmation that names exactly
//     what dies, and only that dialog is allowed to send force:true.
//   · 'unknown' (git could not answer) is treated as dangerous, never as safe.
//   · the bulk action touches ONLY 'safe' rows, and says so on the button.
//   · a live session blocks removal outright — we say "kill it first" instead
//     of firing a POST we know the daemon will refuse.

const LABEL = { safe: 'safe', 'has-work': 'has work', gone: 'gone', unknown: 'unknown' };
// hazard first: what could be destroyed is what you must look at
const RANK = { 'has-work': 0, unknown: 1, safe: 2, gone: 3 };

const verdictOf = (w) => (LABEL[w?.verdict] ? w.verdict : 'unknown');
const num = (n) => (Number.isFinite(Number(n)) ? Number(n) : 0);
const plural = (n, one, many) => `${n} ${n === 1 ? one : (many || `${one}s`)}`;

// last_commit.at may be epoch seconds, epoch ms, or an ISO string.
function ageOf(at, now) {
  if (at == null) return null;
  let t = null;
  if (typeof at === 'number') t = at < 1e12 ? at * 1000 : at;
  else {
    const p = Date.parse(String(at));
    if (!Number.isNaN(p)) t = p;
  }
  if (t == null) return null;
  const d = now - t;
  return d < 0 ? 'just now' : `${human(d)} ago`;
}

// The sentence the human reads before destroying something. It must be exact:
// no "may", no "some" — the counts the daemon measured, and the plain fact that
// this cannot be undone.
function destroyLine(w) {
  if (verdictOf(w) === 'unknown') {
    return 'Git could not read this worktree — it may hold uncommitted changes and commits '
      + 'that exist nowhere else. Removing it deletes them all, and this cannot be undone.';
  }
  const dirty = num(w.dirty);
  const unpushed = num(w.unpushed);
  const bits = [];
  if (dirty > 0) bits.push(plural(dirty, 'uncommitted file'));
  if (unpushed > 0) {
    bits.push(`${plural(unpushed, 'commit')} that ${unpushed === 1 ? 'exists' : 'exist'} nowhere else`);
  }
  if (!bits.length) return 'This deletes the worktree from disk — this cannot be undone.';
  return `This destroys ${bits.join(' and ')} — this cannot be undone.`;
}

function WorktreeRow({ w, now, busy, err, onRemove }) {
  const verdict = verdictOf(w);
  const hazardous = verdict === 'has-work' || verdict === 'unknown';
  const alive = !!w.session_alive;
  const dirty = num(w.dirty);
  const unpushed = num(w.unpushed);
  const files = Array.isArray(w.dirty_files) ? w.dirty_files.filter(Boolean) : [];
  const age = w.last_commit ? ageOf(w.last_commit.at, now) : null;

  const [expand, setExpand] = useState(false);
  const [confirm, setConfirm] = useState(false);
  // ON only where deleting the branch loses nothing: a safe row whose branch is
  // already merged. Everywhere else — and ALWAYS in the hazard confirm — OFF.
  const [delBranch, setDelBranch] = useState(verdict === 'safe' && !!w.merged);

  const branchBox = (hazard) => (
    <label className={`fd-check fd-wtbranch${hazard ? ' hazard' : ''}`}>
      <input
        type="checkbox"
        checked={delBranch}
        disabled={busy}
        onChange={(e) => setDelBranch(e.target.checked)}
      />
      also delete branch <span className="mono">{w.branch}</span>
      {verdict === 'safe' && w.merged && <span className="why">already merged</span>}
    </label>
  );

  return (
    <div className={`fd-wtrow ${verdict}${busy ? ' busy' : ''}`}>
      <div className="r1">
        <span className="cs">{w.callsign || '(no callsign)'}</span>
        <span className={`fd-wtverdict ${verdict}`}>{LABEL[verdict]}</span>
        {w.branch && <span className="br">{w.branch}</span>}
        {w.base && <span className="base">from {w.base}</span>}
        {alive
          ? <span className="fd-wtalive">● session alive</span>
          : (w.spawn_status ? <span className="fd-wtspawn">{w.spawn_status}</span> : null)}
      </div>

      <div className="path" title={w.path}>{w.path}</div>

      <div className="fd-wtev">
        {dirty > 0 && (files.length ? (
          <button
            type="button"
            className="ev haz link"
            aria-expanded={expand}
            title={files.join('\n')}
            onClick={() => setExpand(!expand)}
          >
            {expand ? '▾' : '▸'} {plural(dirty, 'uncommitted file')}
          </button>
        ) : (
          <span className="ev haz">{plural(dirty, 'uncommitted file')}</span>
        ))}

        {unpushed > 0 && (
          w.upstream ? (
            <span className="ev haz">
              {plural(unpushed, 'unpushed commit')} — not in {w.upstream}
            </span>
          ) : (
            <span className="ev haz">
              {plural(unpushed, 'commit')} · never pushed — these commits exist nowhere else
            </span>
          )
        )}

        {verdict === 'safe' && dirty === 0 && unpushed === 0 && (
          <span className="ev ok">
            clean
            {w.merged
              ? ` · merged into ${w.base || 'its base'}`
              : (w.upstream ? ` · everything is pushed to ${w.upstream}` : '')}
            {' '}· nothing would be lost
          </span>
        )}

        {verdict === 'gone' && (
          <span className="ev faint">the directory is already gone — only a stale row is left</span>
        )}

        {verdict === 'unknown' && (
          <span className="ev act">git could not read this worktree — treat it as if it holds work</span>
        )}

        {w.last_commit?.subject && (
          <span className="ev dim last" title={w.last_commit.sha || ''}>
            last: “{w.last_commit.subject}”{age ? ` · ${age}` : ''}
          </span>
        )}
      </div>

      {expand && files.length > 0 && (
        <div className="fd-wtfiles">
          {files.map((f) => <span key={f} className="fd-filechip hot">{f}</span>)}
        </div>
      )}

      {alive && (
        <div className="fd-wtblocked">
          ⚠ {w.callsign || 'That session'} is still running in here, so this cannot be removed —
          kill the session first (☠ on its card), then come back.
        </div>
      )}

      {/* ---- the confirmation: the ONLY thing that may send force:true ---- */}
      {confirm ? (
        <div className="fd-hazardconfirm">
          <div className="warn">⚠ {destroyLine(w)}</div>
          {unpushed > 0 && (
            <div className="sub">
              {w.upstream
                ? `${w.upstream} holds the earlier commits — these ${unpushed} are not in it, and are on no remote anywhere.`
                : `Branch ${w.branch || '(detached)'} was never pushed: there is no copy on any remote, on any machine.`}
            </div>
          )}
          {files.length > 0 && (
            <div className="fd-wtfiles">
              {files.slice(0, 12).map((f) => <span key={f} className="fd-filechip hot">{f}</span>)}
              {files.length > 12 && <span className="fd-filechip">+{files.length - 12} more</span>}
            </div>
          )}
          <div className="sub">
            The directory <span className="mono">{w.path}</span> is deleted from disk. Nothing in it can be recovered.
          </div>
          {w.branch && branchBox(true)}
          <div className="fd-wtfoot">
            <span className="fd-spacer" />
            <button type="button" className="fd-ghostbtn" disabled={busy} onClick={() => setConfirm(false)}>
              Keep it
            </button>
            <button
              type="button"
              className="fd-dangerbtn"
              disabled={busy || alive}
              onClick={() => onRemove(w, { force: true, deleteBranch: delBranch })}
            >
              {busy ? 'Removing…' : 'Destroy it anyway'}
            </button>
          </div>
        </div>
      ) : (
        <div className="fd-wtacts">
          {verdict === 'safe' && w.branch && branchBox(false)}
          <span className="fd-spacer" />
          {verdict === 'safe' && (
            <>
              <span className="hint">nothing to lose</span>
              <button
                type="button"
                className="fd-ghostbtn"
                disabled={busy || alive}
                onClick={() => onRemove(w, { force: false, deleteBranch: delBranch })}
              >
                {busy ? 'Removing…' : 'Remove'}
              </button>
            </>
          )}
          {verdict === 'gone' && (
            <>
              <span className="hint">purging clears the row and prunes git’s metadata — the branch is left alone</span>
              <button
                type="button"
                className="fd-ghostbtn"
                disabled={busy || alive}
                onClick={() => onRemove(w, { force: false, deleteBranch: false })}
              >
                {busy ? 'Purging…' : 'Purge row'}
              </button>
            </>
          )}
          {hazardous && (
            <>
              <span className="hint haz">
                {verdict === 'has-work'
                  ? 'removing this destroys work — you will be told exactly what'
                  : 'git could not vouch for this one — you will be asked to confirm'}
              </span>
              <button
                type="button"
                className="fd-ghostbtn fd-killbtn"
                disabled={busy || alive}
                onClick={() => setConfirm(true)}
              >
                Remove…
              </button>
            </>
          )}
        </div>
      )}

      {w.note && <div className="fd-wtnote">⚠ {w.note}</div>}

      {err && (
        <div className="fd-wterr">
          <div>✗ {typeof err === 'string' ? err : err.reason}</div>
          {/* A worktree is a working directory: a container run inside it can
              leave paths owned by root. Fleet Deck never escalates — it names
              what blocks it and hands over the command. */}
          {err?.blocked_paths?.length > 0 && (
            <>
              <ul className="fd-wtblocked">
                {err.blocked_paths.map((p) => (
                  <li key={p}><code>{p}</code> <span className="own">owned by {err.blocked_owner}</span></li>
                ))}
              </ul>
              <div className="fd-wtfix">
                <code>{err.fix_command}</code>
                <button
                  type="button"
                  className="fd-ghostbtn"
                  onClick={() => copyText(err.fix_command)}
                  title="copy the command"
                >⧉ copy</button>
              </div>
            </>
          )}
        </div>
      )}
    </div>
  );
}

export default function WorktreesModal({ worktrees, loading, error, now, onReload, onRemove, onClose }) {
  const [busyPaths, setBusyPaths] = useState(() => new Set());
  const [errs, setErrs] = useState({}); // path -> the daemon's reason, verbatim
  const [bulkBusy, setBulkBusy] = useState(false);
  const [bulkNote, setBulkNote] = useState(null);

  const rows = useMemo(() => {
    const list = Array.isArray(worktrees) ? [...worktrees] : [];
    return list.sort((a, b) => {
      const r = (RANK[verdictOf(a)] ?? 9) - (RANK[verdictOf(b)] ?? 9);
      return r !== 0 ? r : String(a.path || '').localeCompare(String(b.path || ''));
    });
  }, [worktrees]);

  // The bulk action's universe: 'safe' AND removable. NOTHING else, ever.
  const safeRows = rows.filter((w) => verdictOf(w) === 'safe' && !w.session_alive);
  const hazardN = rows.filter((w) => {
    const v = verdictOf(w);
    return v === 'has-work' || v === 'unknown';
  }).length;

  const markBusy = (path, on) => setBusyPaths((prev) => {
    const next = new Set(prev);
    if (on) next.add(path); else next.delete(path);
    return next;
  });
  const clearErr = (path) => setErrs((prev) => {
    if (!(path in prev)) return prev;
    const next = { ...prev };
    delete next[path];
    return next;
  });

  const doRemove = async (w, opts) => {
    if (busyPaths.has(w.path) || bulkBusy) return;
    setBulkNote(null);
    clearErr(w.path);
    markBusy(w.path, true);
    const res = await onRemove(w.path, opts);
    markBusy(w.path, false);
    if (res.ok) await onReload();
    else setErrs((prev) => ({ ...prev, [w.path]: res }));
  };

  // Sequential, one POST at a time — the daemon is running git per row, and a
  // burst of parallel removals would race on the same repo's index/lock.
  const removeAllSafe = async () => {
    if (bulkBusy || !safeRows.length) return;
    setBulkBusy(true);
    setBulkNote(null);
    let done = 0;
    const failed = [];
    for (const w of safeRows) {
      clearErr(w.path);
      markBusy(w.path, true);
      const res = await onRemove(w.path, { force: false, deleteBranch: false });
      markBusy(w.path, false);
      if (res.ok) done += 1;
      else {
        failed.push(w.path);
        setErrs((prev) => ({ ...prev, [w.path]: res }));
      }
    }
    setBulkBusy(false);
    setBulkNote(failed.length
      ? `removed ${done}/${safeRows.length} — ${failed.length} refused, the reason is on the row`
      : `removed ${done} safe worktree${done === 1 ? '' : 's'} — branches left alone`);
    await onReload();
  };

  const anyBusy = bulkBusy || busyPaths.size > 0;
  const n = rows.length;

  return (
    <div className="fd-composewrap" onClick={anyBusy ? undefined : onClose}>
      <div
        className="fd-compose fd-wtree"
        role="dialog"
        aria-modal="true"
        aria-label="Worktrees"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="fd-wthead">
          <span className="lbl">⑂ WORKTREES</span>
          {n > 0 && <span className={`fd-wtcount${hazardN ? ' haz' : ''}`}>{n}</span>}
          <span className="fd-spacer" />
          <button
            type="button"
            className="fd-ghostbtn fd-wtrefresh"
            disabled={loading || anyBusy}
            onClick={onReload}
          >
            {loading ? 'Reading git…' : '⟳ Refresh'}
          </button>
          <button type="button" className="fd-x" aria-label="Close" disabled={anyBusy} onClick={onClose}>✕</button>
        </div>

        <div className="sub">
          Every spawn with the worktree option gets its own checkout and its own branch. When the agent
          dies the worktree stays on disk — this is where you look at what is inside it and decide.
        </div>

        {error && <div className="fd-spawnerr">✗ {error}</div>}

        {n > 0 && (
          <div className="fd-wtbulk">
            <button
              type="button"
              className="fd-ghostbtn"
              disabled={!safeRows.length || anyBusy}
              onClick={removeAllSafe}
            >
              {bulkBusy ? 'Removing…' : `Remove all safe (${safeRows.length})`}
            </button>
            <span className="note">
              only the {safeRows.length} row{safeRows.length === 1 ? '' : 's'} marked <b>safe</b>.
              {hazardN > 0
                ? ` Nothing with work in it is touched, and neither is anything git couldn’t read — those ${hazardN} stay put.`
                : ' Anything with work in it, or that git couldn’t read, is never touched.'}
              {' '}Branches are left alone.
            </span>
          </div>
        )}
        {bulkNote && <div className="fd-wtbulknote">{bulkNote}</div>}

        <div className="fd-wtlist">
          {worktrees === null && loading && (
            <div className="fd-wtempty">
              <div className="t2">reading git…</div>
            </div>
          )}
          {worktrees !== null && n === 0 && (
            <div className="fd-wtempty">
              <div className="t1">NO WORKTREES</div>
              <div className="t2">
                Spawns with the worktree option create them here — each agent gets a checkout of its own and a
                branch of its own, and this is where you clean them up once it is done.
              </div>
            </div>
          )}
          {rows.map((w) => (
            <WorktreeRow
              key={w.path}
              w={w}
              now={now}
              busy={busyPaths.has(w.path)}
              err={errs[w.path]}
              onRemove={doRemove}
            />
          ))}
        </div>
      </div>
    </div>
  );
}
