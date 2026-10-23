// A mono path that fits its box: whole leading folders give way to "…/", so that a cut never
// lands in the middle of a word (`…/src/shim/reaper.rs`, not `…etdeck-station/src/…`). The box
// keeps the CSS start ellipsis (`direction: rtl`) as the fallback for a file name that alone is
// too long, and for the first paint before the box is measured.
import { useLayoutEffect, useRef, useState } from 'react';

/** The mark in front of a path that lost its leading folders. */
export const PATH_CUT = '…/';

/** `path` in at most `max` characters: whole leading folders dropped behind "…/". When even the
    file name does not fit, "…/" and the file name (the CSS ellipsis cuts the rest). */
export function shortPath(path: string, max: number): string {
  if (path.length <= max) return path;
  const parts = path.split('/');
  for (let i = 1; i < parts.length; i++) {
    const rest = parts.slice(i).join('/');
    if (rest === '') break;
    const cut = `${PATH_CUT}${rest}`;
    if (cut.length <= max || i === parts.length - 1) return cut;
  }
  return path;
}

/** The characters of a mono text that fit `width`: the measured advance of one character. */
export function fitChars(width: number, textWidth: number, textLength: number): number | null {
  if (textLength <= 0 || textWidth <= 0) return null;
  // A small tolerance: a sub-pixel rounding must not drop one more folder.
  return Math.floor((width * textLength) / textWidth + 0.01);
}

/** `path` shortened to the width of its parent box (a one-line, `overflow: hidden` mono box).
    A screen reader reads the whole path. The parent box must have a width that does not depend on
    its text (a block, a grid cell, or a flex item with `flex: 1 1 auto; min-width: 0`): a box as
    wide as its text shrinks to a cut path and never grows back. */
export function PathTail({ path }: { readonly path: string }) {
  const text = useRef<HTMLElement>(null);
  const [max, setMax] = useState<number | null>(null);
  useLayoutEffect(() => {
    const box = text.current?.parentElement;
    if (!box || typeof ResizeObserver === 'undefined') return;
    let live = true;
    const measure = () => {
      const el = text.current;
      if (!live || !el) return;
      const fits = fitChars(
        box.clientWidth,
        el.getBoundingClientRect().width,
        el.textContent?.length ?? 0,
      );
      if (fits !== null) setMax(fits);
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(box);
    // The mono web font changes the advance of a character when it loads.
    void document.fonts?.ready.then(measure);
    return () => {
      live = false;
      observer.disconnect();
    };
  }, []);
  const shown = max === null ? path : shortPath(path, max);
  const cut = shown !== path;
  // One shape for both cases: the measured `<bdi>` stays the same node.
  return (
    <>
      <bdi ref={text} aria-hidden={cut ? 'true' : undefined}>
        {shown}
      </bdi>
      {cut ? <span className="fd-vh">{path}</span> : null}
    </>
  );
}
