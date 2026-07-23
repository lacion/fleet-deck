import React, { useCallback, useMemo } from 'react';
import SessionCard from './SessionCard.jsx';
import { COLS, boardCol, basename, sessionsById, callsignOf } from '../util.js';

// One shared empty array for the "no conflict" case — a fresh `[]` per card
// would defeat React.memo(SessionCard).
const EMPTY = [];

// The board: repo filter chips, sticky column heads, one lane per repo with a
// 5-column card grid (QUEUED / WORKING / VERIFYING / IDLE / OFFLINE — offline
// faded). needsyou sessions render in WORKING with the amber treatment; their
// question lives in the global rail.
//
// M-P4 — this whole component is React.memo'd (see export) and takes NO `now`:
// the 1 s clock reaches only the <Age> leaves via context, so a tick re-renders
// neither the lanes nor any card. The card props are kept referentially stable
// (memoized derivation + stable callbacks) so the memo actually holds.
function BoardLanes({
  sessions, repos, conflicts, mailPending, mailMeta, compact, stale, legacyUpgrade,
  repoFilter, onRepoFilter, ripples, priorities, onOpenSession, onOpenTerm,
  reviving, revivingAll, onRevive, onReviveAll, enablingRemote, onEnableRemote, onKill,
  onToggleWatch, watch, onArmMove, onDisarm, adopting, onRename, onDismiss,
}) {
  // SessionCard calls onOpen(s); the drawer keys off session_id. One stable
  // wrapper for the whole board rather than a fresh closure per card.
  const onOpen = useCallback((s) => onOpenSession(s.session_id), [onOpenSession]);
  // 0.16.0: pre-upgrade sessions carry a per-card "restart me" tag. The daemon
  // hands the ids as an array; a Set keeps per-card lookup O(1) and the
  // reference stable across frames that don't change it.
  const legacySet = useMemo(
    () => new Set(legacyUpgrade?.sessions ?? []),
    [legacyUpgrade],
  );

  // M-P4 — the lane derivation is pure over (sessions, repos, conflicts,
  // repoFilter). Memoizing it keeps the derived Maps/arrays stable across an
  // unrelated re-render so the card props below don't churn.
  const { lanes, chips, colHeads, revivables, conflictFiles, conflictPeers } = useMemo(() => {
    const repoKey = (s) => s.repo_id ?? '(none)';
    const byId = sessionsById(sessions);
    const csOf = (sid) => callsignOf(byId, sid);

    // M-F4 — a session in TWO conflicts must keep BOTH peers. The old code did
    // `set(sid, …)` which OVERWROTE per conflict and silently dropped the
    // earlier peer (unlike conflictFiles, which accumulated). Peers now
    // accumulate into a Set per sid, deduped, then flatten to an array once.
    const cFiles = new Map();    // sid -> [file]
    const cPeerSets = new Map(); // sid -> Set<callsign>
    for (const c of conflicts) {
      for (const sid of c.sessions || []) {
        if (!cFiles.has(sid)) cFiles.set(sid, []);
        cFiles.get(sid).push(c.file || c.rel_path);
        if (!cPeerSets.has(sid)) cPeerSets.set(sid, new Set());
        const set = cPeerSets.get(sid);
        for (const x of c.sessions || []) if (x !== sid) set.add(csOf(x));
      }
    }
    const cPeers = new Map();
    for (const [sid, set] of cPeerSets) cPeers.set(sid, [...set]);

    const visible = repoFilter === 'all' ? sessions : sessions.filter((s) => repoKey(s) === repoFilter);
    const heads = COLS.map((c) => ({
      ...c,
      count: visible.filter((s) => boardCol(s.col) === c.key).length,
    }));

    // v1.5 — dead board-spawned agents whose worktree + transcript survive.
    // The bulk action sits on the OFFLINE column head from 2 revivable cards.
    const revs = visible.filter((s) => s.col === 'offline' && s.spawn?.revivable);

    // lane order = repos snapshot order (first seen), skipping empty lanes
    const laneDefs = repos
      .map((r) => ({ key: r.repo_id ?? '(none)', name: r.repo_name || (r.repo_id ? basename(r.repo_id) : 'no repo') }))
      .filter((l) => visible.some((s) => repoKey(s) === l.key));
    // sessions whose repo never made it into repos[] (defensive)
    for (const s of visible) {
      if (!laneDefs.some((l) => l.key === repoKey(s))) {
        laneDefs.push({ key: repoKey(s), name: s.repo_name || basename(s.repo_id || s.cwd || '') || 'no repo' });
      }
    }

    const laneList = laneDefs.map((l) => {
      const ss = visible.filter((s) => repoKey(s) === l.key);
      const wts = [...new Set(ss.map((s) => s.worktree || 'main'))];
      const laneConf = conflicts.filter((c) => (c.sessions || []).some((sid) => ss.some((s) => s.session_id === sid)));
      return {
        ...l,
        sessions: ss,
        wtNote: wts.length > 1 ? `${wts.length} worktrees` : null,
        meta: `${ss.length} session${ss.length === 1 ? '' : 's'}`,
        conflictNote: laneConf.length
          ? [...new Set(laneConf.map((c) => basename(c.file || c.rel_path)))].join(', ') + ' contested'
          : null,
      };
    });

    const chipList = [
      { id: 'all', label: 'all' },
      ...repos
        .filter((r) => sessions.some((s) => repoKey(s) === (r.repo_id ?? '(none)')))
        .map((r) => ({ id: r.repo_id ?? '(none)', label: r.repo_name || (r.repo_id ? basename(r.repo_id) : 'no repo') })),
    ];

    return {
      lanes: laneList,
      chips: chipList,
      colHeads: heads,
      revivables: revs,
      conflictFiles: cFiles,
      conflictPeers: cPeers,
    };
  }, [sessions, repos, conflicts, repoFilter]);

  return (
    <>
      <div className="fd-repochips">
        <span className="lbl">REPOS</span>
        {chips.map((c) => (
          <button
            key={c.id}
            type="button"
            className={`fd-chip${repoFilter === c.id ? ' on' : ''}`}
            onClick={() => onRepoFilter(c.id)}
          >
            {c.label}
          </button>
        ))}
      </div>
      <div className={`fd-lanes${stale ? ' stale' : ''}`}>
        <div className="fd-colheads">
          {colHeads.map((c) => (
            <div className="h" key={c.key}>
              <span className="l">{c.label}</span>
              <span className="n">{c.count}</span>
              {c.key === 'offline' && onReviveAll && revivables.length >= 2 && (
                <button
                  type="button"
                  className="fd-ghostbtn fd-reviveall"
                  disabled={revivingAll}
                  title="revive every offline card whose worktree + transcript survive"
                  onClick={() => onReviveAll(revivables)}
                >
                  ⟲ {revivingAll ? 'Reviving…' : `Revive all (${revivables.length})`}
                </button>
              )}
            </div>
          ))}
        </div>
        {lanes.map((lane) => (
          <div className="fd-lane" key={lane.key}>
            <div className="fd-lanehead">
              <span className="name">▸ {lane.name}</span>
              {lane.wtNote && <span className="wt">{lane.wtNote}</span>}
              <span className="meta">{lane.meta}</span>
              {lane.conflictNote && <span className="conf">▲ {lane.conflictNote}</span>}
              <span className="rule" />
            </div>
            <div className="fd-lanegrid">
              {COLS.map((c) => (
                <div className="fd-col" key={c.key}>
                  {lane.sessions
                    .filter((s) => boardCol(s.col) === c.key)
                    .map((s) => (
                      <SessionCard
                        key={s.session_id}
                        s={s}
                        compact={compact}
                        mailCount={mailPending[s.session_id] || 0}
                        mailMeta={mailMeta[s.session_id] || null}
                        conflictFiles={conflictFiles.get(s.session_id) || EMPTY}
                        conflictPeers={conflictPeers.get(s.session_id) || EMPTY}
                        ripple={ripples.has(s.session_id)}
                        priority={priorities.has(s.session_id)}
                        onOpen={onOpen}
                        onOpenTerm={onOpenTerm}
                        onRevive={onRevive}
                        reviving={!!(s.spawn && reviving?.has(s.spawn.spawn_id))}
                        onEnableRemote={onEnableRemote}
                        enablingRemote={!!(s.spawn && enablingRemote?.has(s.spawn.spawn_id))}
                        onKill={onKill}
                        onToggleWatch={onToggleWatch}
                        watched={!!watch?.has(s.session_id)}
                        onArmMove={onArmMove}
                        onDisarm={onDisarm}
                        adopting={!!adopting?.has(s.session_id)}
                        onRename={onRename}
                        onDismiss={onDismiss}
                        legacy={legacySet.has(s.session_id)}
                      />
                    ))}
                </div>
              ))}
            </div>
          </div>
        ))}
      </div>
    </>
  );
}

export default React.memo(BoardLanes);
