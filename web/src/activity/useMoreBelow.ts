// The cue of a scroll body (DESIGN-SYSTEM §7, "scroll body fade"): true while the element can
// scroll further down, so that its bottom fades only then.
import { type RefObject, useEffect, useState } from 'react';

/** True while `ref` can scroll further down. `content` stands for what the box holds: a new row
 * changes the scroll height, not the box, so the hook measures again when it changes. */
export function useMoreBelow(ref: RefObject<HTMLElement | null>, content: unknown): boolean {
  const [more, setMore] = useState(false);
  useEffect(() => {
    const el = ref.current;
    if (el === null) return undefined;
    const update = () => setMore(el.scrollTop + el.clientHeight < el.scrollHeight - 1);
    update();
    el.addEventListener('scroll', update, { passive: true });
    const sizes = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(update);
    sizes?.observe(el);
    return () => {
      el.removeEventListener('scroll', update);
      sizes?.disconnect();
    };
  }, [ref, content]);
  return more;
}
