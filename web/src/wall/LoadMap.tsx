// R13 P9 (design §10): the heatmap of the load of each session. One cell for each session,
// sized by `cpu_pct`, with a tooltip for the memory. It has no control and no focus stop.
// It is the last block of the side area and takes the height that the other blocks leave: a cell
// whose bottom edge is below the grid is hidden, and the heading counts the hidden cells.
import { useLayoutEffect, useRef, useState } from 'react';
import { loadHeading, type LoadMapData } from './loadCells';

export function LoadMap({ map }: { readonly map: LoadMapData }) {
  const block = useRef<HTMLDivElement>(null);
  const grid = useRef<HTMLUListElement>(null);
  const [hidden, setHidden] = useState(0);

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
    };
    fit();
    const watcher = new ResizeObserver(fit);
    watcher.observe(el);
    return () => watcher.disconnect();
  });

  return (
    <div className="fd-wall-block fd-wall-loadblock" data-part="load" ref={block}>
      <h2 className="fd-wall-h">{loadHeading(hidden + map.more)}</h2>
      <ul className="fd-wall-load" aria-label="Session load" ref={grid}>
        {map.cells.map((cell) => (
          <li
            key={cell.key}
            data-size={cell.size}
            data-heat={cell.heat}
            title={cell.label}
            aria-label={cell.label}
          >
            <span className="fd-wall-load-name fd-callsign">{cell.name}</span>
            <span className="fd-wall-load-value">{cell.value}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}
