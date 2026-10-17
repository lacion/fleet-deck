// R13 P9 (design §10): the heatmap of the load of each session. One cell for each session,
// sized by `cpu_pct`, with a tooltip for the memory. It has no control and no focus stop.
import type { LoadMapData } from './loadCells';

export function LoadMap({ map }: { readonly map: LoadMapData }) {
  return (
    <>
      <ul className="fd-wall-load" aria-label="Session load">
        {map.cells.map((cell) => (
          <li
            key={cell.key}
            data-size={cell.size}
            data-heat={cell.heat}
            title={cell.label}
            aria-label={cell.label}
          >
            <span className="fd-wall-load-name">{cell.name}</span>
            <span className="fd-wall-load-value">{cell.value}</span>
          </li>
        ))}
      </ul>
      {map.more === 0 ? null : <p className="fd-wall-note">+{map.more} more</p>}
    </>
  );
}
