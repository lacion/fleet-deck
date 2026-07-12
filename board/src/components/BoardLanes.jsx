import React from 'react';
import SessionCard from './SessionCard.jsx';
import { COLS, boardCol, basename } from '../util.js';

// The board: repo filter chips, sticky column heads, one lane per repo with a
// 5-column card grid (QUEUED / WORKING / VERIFYING / IDLE / OFFLINE — offline
// faded). needsyou sessions render in WORKING with the amber treatment; their
// question lives in the global rail.
export default function BoardLanes({
  sessions, repos, conflicts, mailPending, mailMeta, now, compact, stale,
  repoFilter, onRepoFilter, ripples, priorities, onOpenSession, onOpenTerm,
  reviving, revivingAll, onRevive, onReviveAll, enablingRemote, onEnableRemote, onKill,
  onToggleWatch, watch,
}) {
  const repoKey = (s) => s.repo_id ?? '(none)';

  // conflict lookups
  const byId = new Map(sessions.map((s) => [s.session_id, s]));
  const csOf = (sid) => byId.get(sid)?.callsign || sid;
  const conflictFiles = new Map(); // sid -> [file]
  const conflictPeers = new Map(); // sid -> [callsign]
  for (const c of conflicts) {
    for (const sid of c.sessions || []) {
      if (!conflictFiles.has(sid)) conflictFiles.set(sid, []);
      conflictFiles.get(sid).push(c.file || c.rel_path);
      conflictPeers.set(sid, (c.sessions || []).filter((x) => x !== sid).map(csOf));
    }
  }

  const visible = repoFilter === 'all' ? sessions : sessions.filter((s) => repoKey(s) === repoFilter);
  const colHeads = COLS.map((c) => ({
    ...c,
    count: visible.filter((s) => boardCol(s.col) === c.key).length,
  }));

  // v1.5 — dead board-spawned agents whose worktree + transcript survive.
  // The bulk action sits on the OFFLINE column head from 2 revivable cards.
  const revivables = visible.filter((s) => s.col === 'offline' && s.spawn?.revivable);

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

  const lanes = laneDefs.map((l) => {
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

  const chips = [
    { id: 'all', label: 'all' },
    ...repos
      .filter((r) => sessions.some((s) => repoKey(s) === (r.repo_id ?? '(none)')))
      .map((r) => ({ id: r.repo_id ?? '(none)', label: r.repo_name || (r.repo_id ? basename(r.repo_id) : 'no repo') })),
  ];

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
                        now={now}
                        compact={compact}
                        mailCount={mailPending[s.session_id] || 0}
                        mailMeta={mailMeta[s.session_id] || null}
                        conflictFiles={conflictFiles.get(s.session_id) || []}
                        conflictPeers={conflictPeers.get(s.session_id) || []}
                        ripple={(ripples.get(s.session_id) || 0) > now}
                        priority={priorities.has(s.session_id)}
                        onOpen={() => onOpenSession(s.session_id)}
                        onOpenTerm={onOpenTerm ? () => onOpenTerm(s) : undefined}
                        onRevive={onRevive ? () => onRevive(s) : undefined}
                        reviving={!!(s.spawn && reviving?.has(s.spawn.spawn_id))}
                        onEnableRemote={onEnableRemote ? () => onEnableRemote(s) : undefined}
                        enablingRemote={!!(s.spawn && enablingRemote?.has(s.spawn.spawn_id))}
                        onKill={onKill ? () => onKill(s) : undefined}
                        onToggleWatch={onToggleWatch ? () => onToggleWatch(s) : undefined}
                        watched={!!watch?.has(s.session_id)}
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
