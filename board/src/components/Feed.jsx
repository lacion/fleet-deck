import React, { useState } from 'react';
import { hhmmss, classifyTicker, stripTickerEmoji } from '../util.js';

const FILTERS = ['all', 'ask', 'confl', 'mail', 'tool', 'join'];
const TAG_TEXT = { join: 'join', tool: 'tool', ask: 'ask', confl: 'confl', mail: 'mail' };

// Bottom ticker feed — mono, hazard-colored conflict lines, filter chips.
// v1.2: spawn_orphans (fd<port>-* windows with no spawns row) get a one-line
// dim notice — display only, no ops.
export default function Feed({ ticker, orphans }) {
  const [filter, setFilter] = useState('all');
  const rows = (ticker || [])
    .map((e) => ({ at: e.at, tag: classifyTicker(e.msg), msg: stripTickerEmoji(e.msg) }))
    .filter((e) => filter === 'all' || e.tag === filter);
  const orphanN = (orphans || []).length;

  return (
    <div className="fd-feed">
      <div className="fd-feedhead">
        <span className="lbl">FEED</span>
        {FILTERS.map((f) => (
          <button
            key={f}
            type="button"
            className={`fd-chip${filter === f ? ' on' : ''}`}
            onClick={() => setFilter(f)}
          >
            {f}
          </button>
        ))}
      </div>
      {orphanN > 0 && (
        <div className="fd-orphnote" title={(orphans || []).map((o) => o.window).filter(Boolean).join(', ')}>
          {orphanN} unadopted fleet window{orphanN === 1 ? '' : 's'} in tmux
        </div>
      )}
      <div className="fd-feedlines">
        {rows.length === 0 && (
          <div className="fd-feedline"><span className="m">quiet so far</span></div>
        )}
        {rows.map((e, i) => (
          <div className="fd-feedline" key={`${e.at}-${i}`}>
            <span className="t">{hhmmss(e.at)}</span>
            <span className={`tag tag-${e.tag}`}>{TAG_TEXT[e.tag] || '·'}</span>
            <span className={`m${e.tag === 'confl' ? ' hot' : ''}`}>{e.msg}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
