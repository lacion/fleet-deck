import {
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from 'react';
import { useDismiss } from './dismiss';

/**
 * The scroll body fade of a long menu (base.css, DESIGN-SYSTEM §7): `data-scrolled` away from its
 * top, `data-more-below` while rows hide below its bottom edge.
 */
function markEdges(el: HTMLElement) {
  el.toggleAttribute('data-scrolled', el.scrollTop > 0);
  el.toggleAttribute('data-more-below', el.scrollHeight - el.scrollTop - el.clientHeight > 1);
}

/**
 * The edge marks of `markEdges` on a scroll body that is not a menu (the desktop sidebar list): set
 * at once, after each scroll, and when the body or a child of it changes size (a fold opens, rows
 * come or go, the window resizes). A child that comes later is watched too.
 */
export function useScrollEdges(ref: RefObject<HTMLElement | null>, on = true): void {
  useEffect(() => {
    const el = ref.current;
    if (!on || el === null) return undefined;
    const mark = () => markEdges(el);
    mark();
    el.addEventListener('scroll', mark, { passive: true });
    const sizes = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(mark);
    const watch = () => {
      if (sizes === null) return;
      sizes.disconnect();
      sizes.observe(el);
      for (const child of Array.from(el.children)) sizes.observe(child);
    };
    watch();
    const children =
      typeof MutationObserver === 'undefined'
        ? null
        : new MutationObserver(() => {
            watch();
            mark();
          });
    children?.observe(el, { childList: true });
    return () => {
      el.removeEventListener('scroll', mark);
      sizes?.disconnect();
      children?.disconnect();
    };
  }, [ref, on]);
}

/**
 * A `<details>` popover menu. Escape or a press outside it closes it; Escape returns the focus
 * to the summary. The body gets a `close` function.
 */
export function Menu(props: {
  readonly summary: ReactNode;
  readonly label?: string;
  readonly summaryClass?: string;
  /** `toggle`: the summary ends in the chevron of base.css that turns when the menu opens. */
  readonly variant?: 'toggle';
  readonly title?: string;
  /**
   * A dim layer under the open body (a bottom sheet, the phone account menu): a tap on it closes
   * the menu. It sits inside the menu, so the press that closes it does not reach the page
   * under it.
   */
  readonly scrim?: boolean;
  readonly onClosed?: () => void;
  readonly children: (close: () => void) => ReactNode;
}) {
  const ref = useRef<HTMLDetailsElement>(null);
  const body = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const close = () => {
    if (ref.current) ref.current.open = false;
  };
  useDismiss(ref, open, close);
  // The rows can change while the menu is open (a confirm step): mark the edges after each render.
  useLayoutEffect(() => {
    if (open && body.current) markEdges(body.current);
  });
  const onKeyDown = (event: KeyboardEvent<HTMLDetailsElement>) => {
    if (event.key !== 'Escape' || !ref.current?.open) return;
    event.stopPropagation();
    close();
    ref.current.querySelector('summary')?.focus();
  };
  return (
    <details
      className="fd-menu"
      ref={ref}
      onKeyDown={onKeyDown}
      onToggle={(event) => {
        setOpen(event.currentTarget.open);
        if (!event.currentTarget.open) props.onClosed?.();
      }}
    >
      <summary
        className={props.summaryClass}
        data-variant={props.variant}
        aria-label={props.label}
        title={props.title}
      >
        {props.summary}
      </summary>
      {props.scrim === true ? (
        <div className="fd-menu-scrim" aria-hidden="true" onClick={close} />
      ) : null}
      <div className="fd-menu-body" ref={body} onScroll={(event) => markEdges(event.currentTarget)}>
        {props.children(close)}
      </div>
    </details>
  );
}
