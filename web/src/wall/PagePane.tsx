// B17 §3.2.7 (design `docs/v2/design/wall-control.md`): the page mode of the wall.
// - A wall display shows the allowed page in a closed frame: `credentialless` (no cookies, no storage
//   of the wall), a sandbox without top navigation and popups, an empty permissions policy and no
//   referrer. The wall document ignores `message` events (this file adds no listener for them).
// - The owner's `#/wall` and the mirror of the Remote page never load the page: they show a card.
// The policy of the document is fixed when it loads (§3.2.6): a page directive of a new generation
// reloads the document once, with `?page_gen=<generation>`, before the frame exists.
import { useCallback, useEffect, useRef, useState } from 'react';
import { DISPLAY_PATH, displayPage, pagePath } from '../display/api';
import type { DisplayPage, PageFit, WallPage as WallPageEntry } from '../gen';
import { useSettings } from '../settings/context';
import { FRAME_TENTHS_MAX, LONG_FIT_SCREENS, longFrameTop, scrollBehavior } from './control';
import type { DisplayEnv } from './displayChanges';

/** The `sandbox` of the page frame: scripts, its own origin and forms; no top navigation, no popup. */
export const FRAME_SANDBOX = 'allow-scripts allow-same-origin allow-forms';
/** The permissions policy of the page frame: none. */
export const FRAME_ALLOW = '';

export const UNSUPPORTED_TEXT = 'This browser cannot show pages on a wall safely.';
export const BOARD_TEXT =
  'This is the address of the board. A wall cannot show the board in a frame.';
export const BLOCKED_TEXT = 'The wall could not load this page. Reload the wall.';
export const UNAVAILABLE_TEXT = 'This page is not available.';

/** True when the frame element has the `credentialless` attribute (the prototype lists it). */
export const supportsCredentialless = (proto: object): boolean => 'credentialless' in proto;

/** `supportsCredentialless` for this browser; false without a DOM. */
export const browserSupports = (): boolean =>
  typeof HTMLIFrameElement !== 'undefined' && supportsCredentialless(HTMLIFrameElement.prototype);

/**
 * The document to load for a page directive of `generation`, or `null` when the document URL has
 * the query `page_gen=<generation>` already (the document then holds the page origin). `hash` is
 * `location.hash`.
 */
export function reloadTarget(search: string, hash: string, generation: number): string | null {
  return new URLSearchParams(search).get('page_gen') === String(generation)
    ? null
    : `${DISPLAY_PATH}?page_gen=${generation}${hash}`;
}

/**
 * Calls `replace` for `target` unless the last call was for the same target. `null` (no reload
 * needed) calls nothing. Returns the last target: the wall reloads at most once for each page
 * generation.
 */
export function reloadOnce(
  last: string | null,
  target: string | null,
  replace: (url: string) => void,
): string | null {
  if (target === null || last === target) return last;
  replace(target);
  return target;
}

/** True when `url` has the origin of this document (the board): a wall cannot frame the board. */
export function sameOrigin(url: string, origin: string): boolean {
  try {
    return new URL(url).origin === origin;
  } catch {
    return false;
  }
}

/** What `loadFrame` needs from the frame element. */
export type FrameLike = {
  setAttribute(name: string, value: string): void;
  src: string;
};

/**
 * Loads `url` in the frame. The attribute `credentialless` must be on the element before the first
 * navigation, so it is set first. React does not know the attribute.
 */
export function loadFrame(frame: FrameLike, url: string): void {
  frame.setAttribute('credentialless', '');
  frame.src = url;
}

/** True for the report of a document policy that refused the frame (`frame-src`). */
export const isFrameSrcViolation = (event: { readonly effectiveDirective?: string }): boolean =>
  event.effectiveDirective === 'frame-src';

/** The scroll position of a page pane: the long fit follows `tenths`; one screen stays at 0. */
export const pageScrollTop = (fit: PageFit, tenths: number, clientHeight: number): number =>
  fit === 'long' ? longFrameTop(tenths, clientHeight) : 0;

/** The screen number on the card of a long page: "Screen N of 8" (`N` is 1 to 8). */
export const screenOf = (tenths: number): number =>
  Math.floor(Math.min(FRAME_TENTHS_MAX, Math.max(0, tenths)) / 10) + 1;

/** The host of an origin, as the deck audit writes it: no scheme and no port. */
export function hostOf(origin: string): string {
  const rest = origin.includes('://') ? origin.slice(origin.indexOf('://') + 3) : origin;
  if (rest.startsWith('[')) {
    const end = rest.indexOf(']');
    return end < 0 ? rest : rest.slice(0, end + 1);
  }
  return rest.split(':')[0] ?? rest;
}

/** The frame, with the exact `sandbox` and `allow`. It has no `src`: `loadFrame` sets it. */
export function PageFrame({
  label,
  frameRef,
}: {
  readonly label: string;
  readonly frameRef?: ((el: HTMLIFrameElement | null) => void) | undefined;
}) {
  return (
    <iframe
      className="fd-wall-pageframe"
      title={label}
      ref={frameRef}
      sandbox={FRAME_SANDBOX}
      allow={FRAME_ALLOW}
      referrerPolicy="no-referrer"
    />
  );
}

/** What a wall display shows for the page of its directive. */
export type DisplayState =
  | { readonly kind: 'loading' }
  | { readonly kind: 'failed' }
  | { readonly kind: 'ready'; readonly page: DisplayPage };

/** The pane for a state: a note, or the section with the frame. `frameRef` loads the frame. */
export function PageView({
  state,
  supported,
  origin,
  blocked,
  frameRef,
  sectionRef,
}: {
  readonly state: DisplayState;
  readonly supported: boolean;
  /** The origin of the board document. */
  readonly origin: string;
  /** The document policy refused the frame (`frame-src`). */
  readonly blocked: boolean;
  readonly frameRef?: ((el: HTMLIFrameElement | null) => void) | undefined;
  readonly sectionRef?: ((el: HTMLElement | null) => void) | undefined;
}) {
  if (state.kind === 'loading') return <p className="fd-wall-note">Loading…</p>;
  if (state.kind === 'failed') return <p className="fd-wall-note">{UNAVAILABLE_TEXT}</p>;
  if (!supported) return <p className="fd-wall-note">{UNSUPPORTED_TEXT}</p>;
  if (sameOrigin(state.page.url, origin)) return <p className="fd-wall-note">{BOARD_TEXT}</p>;
  if (blocked) return <p className="fd-wall-note">{BLOCKED_TEXT}</p>;
  return (
    <section
      className="fd-wall-page"
      data-fit={state.page.fit}
      data-wall-scroll="page"
      aria-label={state.page.label}
      ref={sectionRef}
    >
      <PageFrame label={state.page.label} frameRef={frameRef} />
    </section>
  );
}

/**
 * The page pane of a wall display. It reads the page of the directive for each new generation, after
 * the reload check of §3.2.6, and shows the frame.
 */
export function DisplayPagePane({
  pageId,
  env,
  tenths,
  where,
  replace,
  supported,
  origin,
}: {
  readonly pageId: string;
  readonly env: DisplayEnv;
  /** `frame_tenths` of the remote state: the position of a long page. */
  readonly tenths: number;
  /** `location.search` and `location.hash` (tests give their own). */
  readonly where?: { readonly search: string; readonly hash: string };
  /** `location.replace` (tests give their own). */
  readonly replace?: (url: string) => void;
  readonly supported?: boolean;
  /** The origin of the board document (tests give their own). */
  readonly origin?: string;
}) {
  const generation = env.generation ?? null;
  const here =
    where ??
    (typeof window === 'undefined'
      ? { search: '', hash: '' }
      : { search: window.location.search, hash: window.location.hash });
  const target = generation === null ? null : reloadTarget(here.search, here.hash, generation);
  const [state, setState] = useState<DisplayState>({ kind: 'loading' });
  const [blocked, setBlocked] = useState(false);
  const reloaded = useRef<string | null>(null);
  const get = env.get;
  // The document reloads once for each page generation, before the frame exists.
  useEffect(() => {
    reloaded.current = reloadOnce(
      reloaded.current,
      target,
      replace ?? ((url: string) => window.location.replace(url)),
    );
  }, [target, replace]);
  useEffect(() => {
    if (generation === null || target !== null) return undefined;
    let live = true;
    setState({ kind: 'loading' });
    setBlocked(false);
    get(pagePath(pageId)).then(
      (v) => {
        if (!live) return;
        try {
          setState({ kind: 'ready', page: displayPage(v) });
        } catch {
          setState({ kind: 'failed' });
        }
      },
      () => live && setState({ kind: 'failed' }),
    );
    return () => {
      live = false;
    };
  }, [get, pageId, generation, target]);
  // The directive of the document changed during the reload, or the entry changed: no second reload.
  useEffect(() => {
    const on = (event: Event) => {
      if (isFrameSrcViolation(event as SecurityPolicyViolationEvent)) setBlocked(true);
    };
    document.addEventListener('securitypolicyviolation', on);
    return () => document.removeEventListener('securitypolicyviolation', on);
  }, []);
  const url = state.kind === 'ready' ? state.page.url : null;
  // One ref callback for one URL: a new function on each render would load the frame again.
  const frameRef = useCallback(
    (el: HTMLIFrameElement | null) => {
      if (el !== null && url !== null) loadFrame(el, url);
    },
    [url],
  );
  const section = useRef<HTMLElement | null>(null);
  const sectionRef = useCallback((el: HTMLElement | null) => {
    section.current = el;
  }, []);
  const fit = state.kind === 'ready' ? state.page.fit : 'screen';
  useEffect(() => {
    const el = section.current;
    if (el === null || fit !== 'long') return;
    el.scrollTo({
      top: pageScrollTop(fit, tenths, el.clientHeight),
      behavior: scrollBehavior(),
    });
  }, [fit, tenths, state.kind, blocked]);
  if (generation === null || target !== null) return <p className="fd-wall-note">Loading…</p>;
  return (
    <PageView
      state={state}
      supported={supported ?? browserSupports()}
      origin={origin ?? (typeof window === 'undefined' ? '' : window.location.origin)}
      blocked={blocked}
      frameRef={frameRef}
      sectionRef={sectionRef}
    />
  );
}

/** The card of the owner wall and of the mirror: the name, the host and, for a long page, the screen. */
export function PageCard({
  page,
  tenths,
}: {
  readonly page: Pick<WallPageEntry, 'label' | 'origin' | 'fit'>;
  readonly tenths: number;
}) {
  return (
    <section className="fd-wall-pagecard" aria-label={page.label}>
      <h2 className="fd-wall-h">{page.label}</h2>
      <p className="fd-wall-pagehost">{hostOf(page.origin)}</p>
      {page.fit === 'long' ? (
        <p className="fd-wall-note">{`Screen ${screenOf(tenths)} of ${LONG_FIT_SCREENS}`}</p>
      ) : null}
    </section>
  );
}

export type OwnerState =
  | { readonly kind: 'loading' }
  | { readonly kind: 'failed' }
  | { readonly kind: 'ready'; readonly page: WallPageEntry };

/** The card for a state. */
export function OwnerPageView({
  state,
  tenths,
}: {
  readonly state: OwnerState;
  readonly tenths: number;
}) {
  if (state.kind === 'loading') return <p className="fd-wall-note">Loading…</p>;
  if (state.kind === 'failed') return <p className="fd-wall-note">{UNAVAILABLE_TEXT}</p>;
  return <PageCard page={state.page} tenths={tenths} />;
}

/** The page pane of the owner wall and of the mirror: it reads the allow-list (owner route) once. */
export function OwnerPagePane({
  pageId,
  tenths,
}: {
  readonly pageId: string;
  readonly tenths: number;
}) {
  const displays = useSettings()?.displays;
  const [state, setState] = useState<OwnerState>({ kind: 'loading' });
  useEffect(() => {
    if (displays === undefined) {
      setState({ kind: 'failed' });
      return undefined;
    }
    let live = true;
    setState({ kind: 'loading' });
    displays.wallPages().then(
      (list) => {
        if (!live) return;
        const page = list.pages.find((p) => p.page_id === pageId);
        setState(page === undefined ? { kind: 'failed' } : { kind: 'ready', page });
      },
      () => live && setState({ kind: 'failed' }),
    );
    return () => {
      live = false;
    };
  }, [displays, pageId]);
  return <OwnerPageView state={state} tenths={tenths} />;
}
