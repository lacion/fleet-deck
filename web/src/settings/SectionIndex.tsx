// The section index of a settings tab (a new pattern, DESIGN-SYSTEM §4.7): one link for each
// section of the tab, at the right of the column on a wide screen (settings.css hides it below
// 1600 px). The section in view has `aria-current` and the selection bar. It reads the sections
// from the page, so the cards (Tailnet, Omarchy, Desktop tools) count when they show.
import { type RefObject, useEffect, useState } from 'react';

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

export function SectionIndex({ body }: { readonly body: RefObject<HTMLElement | null> }) {
  const [items, setItems] = useState<readonly Item[]>([]);
  const [current, setCurrent] = useState<string | null>(null);

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

  // The section in view: the last one whose top passed the upper third of the window.
  useEffect(() => {
    const el = body.current;
    if (el === null || items.length === 0 || typeof IntersectionObserver === 'undefined') {
      return undefined;
    }
    const seen = new Map<string, boolean>();
    const watch = new IntersectionObserver(
      (entries) => {
        for (const e of entries) seen.set(e.target.id, e.isIntersecting);
        const first = items.find((it) => seen.get(it.id) === true);
        if (first !== undefined) setCurrent(first.id);
      },
      { rootMargin: '0px 0px -66% 0px' },
    );
    for (const it of items) {
      const section = document.getElementById(it.id);
      if (section !== null) watch.observe(section);
    }
    return () => watch.disconnect();
  }, [body, items]);

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
                const section = document.getElementById(it.id);
                section?.scrollIntoView({ block: 'start' });
                setCurrent(it.id);
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
