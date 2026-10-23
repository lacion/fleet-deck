// R13 P9 (design §10): the heatmap of the CPU of each session. One cell for each busy session,
// named as its tile (the title), sized by `cpu_pct`, with a tooltip for the memory. It has no
// control and no focus stop. It is the last block of the side area and takes the height that the
// other blocks leave: a cell whose bottom edge is below the grid is hidden. The heading counts
// the sessions without a cell (idle, hidden or over the limit) in a `+3` `.fd-count` pill after
// its name (§5: a count of hidden items, never "(+3 more)"). When no cell fits, the whole block
// is hidden.
import { useLayoutEffect, useRef, useState } from 'react';
import { countText } from '../ui/format';
import { LOAD_TITLE, type LoadMapData } from './loadCells';

/**
 * The sessions that have no cell: a neutral `+N` pill after the heading. The plus says "beyond
 * these cells" (a bare number read as the total of the cells under it); its name says the rest.
 */
export function LoadCount({ n }: { readonly n: number }) {
  if (n <= 0) return null;
  return (
    <span className="fd-count">
      {`+${countText(n)}`}
      <span className="fd-vh"> more sessions</span>
    </span>
  );
}

export function LoadMap({ map }: { readonly map: LoadMapData }) {
  const block = useRef<HTMLDivElement>(null);
  const grid = useRef<HTMLUListElement>(null);
  const [hidden, setHidden] = useState(0);
  // No cell fits (the blocks above take the side): the block keeps its place but shows nothing,
  // not a heading cut through its letters.
  const [none, setNone] = useState(false);

  // After each render and on a resize: hide the cells that do not fit.
  useLayoutEffect(() => {
    const el = block.current;
    const list = grid.current;
    if (el === null || list === null || typeof ResizeObserver === 'undefined') return undefined;
    const fit = () => {
      const cells = Array.from(list.children) as HTMLElement[];
      for (const cell of cells) cell.hidden = false;
      let over = 0;
      for (const cell of cells) {
        if (cell.offsetTop + cell.offsetHeight > list.clientHeight) {
          cell.hidden = true;
          over += 1;
        }
      }
      setHidden((now) => (now === over ? now : over));
      setNone(cells.length > 0 && over === cells.length);
    };
    fit();
    const watcher = new ResizeObserver(fit);
    watcher.observe(el);
    return () => watcher.disconnect();
  });

  return (
    <div
      className="fd-wall-block fd-wall-loadblock"
      data-part="load"
      data-fit={none ? 'none' : undefined}
      ref={block}
    >
      <h2 className="fd-wall-h">
        {LOAD_TITLE}
        <LoadCount n={hidden + map.more} />
      </h2>
      <ul className="fd-wall-load" aria-label={LOAD_TITLE} ref={grid}>
        {map.cells.map((cell) => (
          <li
            key={cell.key}
            data-size={cell.size}
            data-heat={cell.heat}
            title={cell.label}
            aria-label={cell.label}
          >
            <span className={cell.callsign ? 'fd-wall-load-name fd-callsign' : 'fd-wall-load-name'}>
              {cell.name}
            </span>
            <span className="fd-wall-load-value">{cell.value}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}
