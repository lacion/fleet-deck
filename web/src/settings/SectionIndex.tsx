// The section index of a settings tab (a new pattern, DESIGN-SYSTEM §4.7): one link for each
// section of the tab, at the right of the column on a wide screen (settings.css hides it below
// 1400 px). The section in view has `aria-current` and the selection bar. It reads the sections
// from the page, so the cards (Tailnet, Omarchy, Desktop tools) count when they show.
import { type RefObject, useEffect, useRef, useState } from 'react';

type Item = { readonly id: string; readonly title: string };

const sameItems = (a: readonly Item[], b: readonly Item[]) =>
  a.length === b.length && a.every((x, i) => x.id === b[i]?.id && x.title === b[i]?.title);

/** The sections of the tab body, in page order. */
function readItems(body: HTMLElement): Item[] {
  return [...body.querySelectorAll<HTMLElement>(':scope > section.fd-set-sec[id]')].map((s) => ({
    id: s.id,
    title: s.getAttribute('aria-label') ?? s.id,
  }));
}

/**
 * The reading line, in pixels from the top of the window: a third of a short window, at most
 * 120px. A section that starts above it is in view; the line sits near the top, so that a short
 * section at the top of the view (Omarchy, General) stays the current one while the next one
 * starts lower on the screen.
 */
export const readingLine = (height: number): number => Math.min(height * 0.33, 120);

/**
 * The section in view: the last one whose top is at or above the reading line (`tops` in page
 * order, in pixels from the top of the window). Before the first section reaches the line, the
 * first one. At the end of the scroll (`end`, the window `height`), the last sections can never
 * reach the line: then the last one that shows on the screen.
 */
export function currentIndex(
  tops: readonly number[],
  line: number,
  end?: { readonly height: number },
): number {
  let at = 0;
  tops.forEach((top, i) => {
    if (top <= line || (end !== undefined && top < end.height)) at = i;
  });
  return at;
}

/** The element that scrolls the tab: the nearest ancestor that scrolls, else the page. */
function scrollerOf(el: HTMLElement): Element {
  for (let p = el.parentElement; p !== null; p = p.parentElement) {
    const overflow = getComputedStyle(p).overflowY;
    if ((overflow === 'auto' || overflow === 'scroll') && p.scrollHeight > p.clientHeight) return p;
  }
  return document.scrollingElement ?? document.documentElement;
}

/** After a click on a link, the spy waits for the end of the scroll (or this long without it). */
const HOLD_MS = 400;

export function SectionIndex({ body }: { readonly body: RefObject<HTMLElement | null> }) {
  const [items, setItems] = useState<readonly Item[]>([]);
  const [current, setCurrent] = useState<string | null>(null);
  // While a click scrolls to its section, the spy does not move the mark (`held`); after it, the
  // clicked link keeps the mark until the owner scrolls again (`pinned`), also when a card that
  // loads late changes the list of sections.
  const held = useRef(false);
  const pinned = useRef(false);
  const release = useRef<(() => void) | null>(null);
  useEffect(() => () => release.current?.(), []);

  // The list follows the sections: a card shows after its read, a section can go.
  useEffect(() => {
    const el = body.current;
    if (el === null) return undefined;
    const read = () =>
      setItems((cur) => {
        const next = readItems(el);
        return sameItems(cur, next) ? cur : next;
      });
    read();
    if (typeof MutationObserver === 'undefined') return undefined;
    const watch = new MutationObserver(read);
    watch.observe(el, { childList: true });
    return () => watch.disconnect();
  }, [body]);

  // The section in view, measured at most once a frame on a scroll (of the page or of any column
  // that holds it) and on a resize.
  useEffect(() => {
    const el = body.current;
    if (el === null || items.length === 0 || typeof window === 'undefined') return undefined;
    let frame = 0;
    const measure = () => {
      frame = 0;
      if (held.current || pinned.current) return;
      const tops = items.map(
        (it) => document.getElementById(it.id)?.getBoundingClientRect().top ?? Infinity,
      );
      const scroller = scrollerOf(el);
      const atEnd =
        scroller.scrollTop > 0 &&
        scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 2;
      const height = window.innerHeight;
      const at = items[currentIndex(tops, readingLine(height), atEnd ? { height } : undefined)];
      if (at !== undefined) setCurrent(at.id);
    };
    const soon = () => {
      if (frame === 0) frame = requestAnimationFrame(measure);
    };
    // A scroll of the column that holds the tab (not the one of a click, not a tab row or a
    // table that scrolls sideways) ends the mark of a click.
    const scrolled = (e: Event) => {
      const target = e.target;
      if (held.current || (target instanceof Node && !target.contains(el))) return;
      pinned.current = false;
      soon();
    };
    measure();
    document.addEventListener('scroll', scrolled, { capture: true, passive: true });
    window.addEventListener('resize', soon, { passive: true });
    return () => {
      if (frame !== 0) cancelAnimationFrame(frame);
      document.removeEventListener('scroll', scrolled, { capture: true });
      window.removeEventListener('resize', soon);
    };
  }, [body, items]);

  /** Marks the clicked section and holds the mark until its scroll ends. */
  const hold = (id: string) => {
    release.current?.();
    setCurrent(id);
    held.current = true;
    pinned.current = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const end = () => {
      clearTimeout(timer);
      document.removeEventListener('scrollend', end, { capture: true });
      held.current = false;
      release.current = null;
    };
    release.current = end;
    // A browser without scrollend (Safari) ends the hold after a time; with it, the end of the
    // scroll does, and the longer timer covers a section that is already in place (no scroll).
    const scrollEnd = typeof document.documentElement.onscrollend !== 'undefined';
    if (scrollEnd) document.addEventListener('scrollend', end, { capture: true });
    timer = setTimeout(end, scrollEnd ? HOLD_MS * 3 : HOLD_MS);
  };

  if (items.length < 2) return null;
  return (
    <nav className="fd-set-toc" aria-label="Sections">
      <ol>
        {items.map((it) => (
          <li key={it.id}>
            <a
              href={`#${it.id}`}
              className="fd-selbar"
              aria-current={current === it.id ? 'true' : undefined}
              onClick={(e) => {
                // The board routes by the hash: scroll, do not navigate.
                e.preventDefault();
                hold(it.id);
                document.getElementById(it.id)?.scrollIntoView({ block: 'start' });
              }}
            >
              {it.title}
            </a>
          </li>
        ))}
      </ol>
    </nav>
  );
}
