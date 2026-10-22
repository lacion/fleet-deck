// Mono paths of the session that fit their room by whole folders: a cut drops leading folders behind
// "…/" and never lands inside a name (`…/fleet-deck-worktrees/v2`, `…/src/shim/reaper.rs`).
//
// - FolderChip: the folder chip of the session header (§4.5). The chip is as wide as its text; the
//   room is measured on the place row, whose width never depends on the folder text (`flex: 1 1
//   auto` as the one growing item of the chips row), so a cut path grows back when the window grows.
//   (PathTail measures its own box, which shrinks with the cut text.)
// - FilePath: a changed file of the Properties panel as the path of a diff file header (§3
//   `.fd-dl-path`): the folders (`--dim`) give way by whole names, the file name never shrinks.
import { useLayoutEffect, useRef, useState } from 'react';
import { PATH_CUT, shortPath } from '../chat/PathTail';
import { Folder, Icon } from '../ui/icons';
import { homeShort } from './CopyButton';

/** The characters of a mono text that fit `room` px at `advance` px per character; `null` without an advance. */
export function roomChars(room: number, advance: number): number | null {
  if (!(advance > 0)) return null;
  // A small tolerance: a sub-pixel rounding must not drop one more folder.
  return Math.max(0, Math.floor(room / advance + 0.01));
}

/**
 * The folders of a file path (`crates/x/src/`) in at most `max` characters: whole leading folders
 * give way to "…/" (`…/src/`). Even the last folder does not fit: "…/" alone.
 */
export function shortDir(dir: string, max: number): string {
  if (dir.length <= max) return dir;
  const short = shortPath(dir.replace(/\/$/, ''), Math.max(0, max - 1));
  const shown = `${short}/`;
  return shown.length <= max ? shown : PATH_CUT;
}

/** Runs `measure` after every render and when the observed box changes size or the font loads. */
function useMeasure(box: () => Element | null | undefined, measure: () => void): void {
  const latest = useRef(measure);
  latest.current = measure;
  // A new render can change what the room holds (the other chips, the path): measure again.
  useLayoutEffect(() => {
    latest.current();
  });
  useLayoutEffect(() => {
    const el = box();
    if (!el || typeof ResizeObserver === 'undefined') return undefined;
    let live = true;
    const run = () => {
      if (live) latest.current();
    };
    const observer = new ResizeObserver(run);
    observer.observe(el);
    // The mono web font changes the advance of a character when it loads.
    void document.fonts?.ready.then(run);
    return () => {
      live = false;
      observer.disconnect();
    };
  }, []);
}

/** The folder of a session as a chip of the header place row (or of the phone menu). */
export function FolderChip({ cwd }: { readonly cwd: string }) {
  const path = homeShort(cwd);
  const chip = useRef<HTMLSpanElement>(null);
  const [max, setMax] = useState<number | null>(null);
  useMeasure(
    () => chip.current?.parentElement,
    () => {
      const el = chip.current;
      const place = el?.parentElement;
      const text = el?.querySelector('bdi');
      const box = text?.parentElement;
      if (!el || !place || !text || !box) return;
      const length = text.textContent?.length ?? 0;
      const width = text.getBoundingClientRect().width;
      if (length === 0 || width === 0) return;
      const style = getComputedStyle(place);
      const gap = Number.parseFloat(style.columnGap) || 0;
      // In a wrapping place (the phone menu) the chip can have a line of its own.
      let others = 0;
      if (style.flexWrap !== 'wrap') {
        for (const child of place.children) {
          if (child !== el) others += child.getBoundingClientRect().width + gap;
        }
      }
      // The padding, the icon and the gap of the chip: all of it but the text box.
      const chrome = el.getBoundingClientRect().width - box.getBoundingClientRect().width;
      const fits = roomChars(place.clientWidth - others - chrome, width / length);
      if (fits !== null) setMax((now) => (now === fits ? now : fits));
    },
  );
  const shown = max === null ? path : shortPath(path, max);
  const cut = shown !== path;
  return (
    <span ref={chip} className="fd-badge fd-sh-chip" data-mono="" data-folder="" title={cwd}>
      <Icon of={Folder} size={13} />
      <span>
        <bdi aria-hidden={cut ? 'true' : undefined}>{shown}</bdi>
        {cut ? <span className="fd-vh">{path}</span> : null}
      </span>
    </span>
  );
}

/**
 * A changed file as the folders and the name of a diff file header, inside a `.fd-dl-path` box
 * whose width never depends on its text (a `flex: 1` cell). The folders drop whole names to fit.
 */
export function FilePath({ path }: { readonly path: string }) {
  const at = path.lastIndexOf('/') + 1;
  const dir = path.slice(0, at);
  const name = path.slice(at);
  const base = useRef<HTMLSpanElement>(null);
  const [max, setMax] = useState<number | null>(null);
  useMeasure(
    () => base.current?.parentElement,
    () => {
      const el = base.current;
      const box = el?.parentElement;
      if (!el || !box || name.length === 0) return;
      const fits = roomChars(box.clientWidth, el.getBoundingClientRect().width / name.length);
      if (fits !== null) setMax((now) => (now === fits ? now : fits));
    },
  );
  const shown = max === null ? dir : shortDir(dir, max - name.length);
  // A screen reader reads the whole path once: the two boxes are flex items, and an accessible
  // name puts a space between them ("src/ a.ts").
  return (
    <>
      {dir === '' ? null : (
        <span className="fd-dl-dir" aria-hidden="true">
          <bdi>{shown}</bdi>
        </span>
      )}
      <span ref={base} className="fd-dl-base" aria-hidden="true">
        {name}
      </span>
      <span className="fd-vh">{path}</span>
    </>
  );
}
