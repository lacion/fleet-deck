import { type ReactNode, type RefObject, useEffect, useRef } from 'react';
import {
  ChevronLeft,
  Ellipsis,
  Icon,
  Layers,
  LayoutGrid,
  type LucideIcon,
  PanelLeft,
  Plus,
  X,
} from '../ui/icons';
import { Mark } from '../ui/Mark';
import { usePeople } from '../sharing/context';
import { MemberBanner } from '../sharing/MemberBanner';
import type { Chrome, Notice } from './chrome';
import { failureText, type StartFailure } from './failures';
import { trapTab, useFocusReturn } from './focus';
import { useOverlay } from './overlay';
import type { Route } from './routes';
import type { Layout as LayoutName } from './viewport';

type Props = {
  readonly layout: LayoutName;
  readonly route: Route;
  /** The `Sidebar`: pinned on desktop, a drawer on tablet. */
  readonly sidebar: ReactNode;
  readonly drawerOpen: boolean;
  readonly onDrawer: (open: boolean) => void;
  readonly notices: readonly Notice[];
  /** A strip under the notices (the one-time help notice). */
  readonly banner?: ReactNode;
  /** The sessions that did not start (R7.1), one banner for each. */
  readonly failures?: readonly StartFailure[];
  readonly onOpenFailure?: (failure: StartFailure) => void;
  readonly onDismissFailure?: (failure: StartFailure) => void;
  /** The terminal dock under the route (desktop and tablet). */
  readonly dock: ReactNode;
  /** The back button of the phone's terminals screen. */
  readonly onTerminalsBack: () => void;
  /** Dialogs that sit above the layout (help, palette). */
  readonly overlays: ReactNode;
  /** The connection of the board: the Live pill of the phone top bar. */
  readonly chrome?: Chrome | undefined;
  readonly children: ReactNode;
};

/**
 * Keeps `--fd-dock-h` on the main column: the height of its terminal dock. Next to the ops panel
 * of the fleet home the dock lies over the main column (shell.css), and the home leaves this room.
 */
function useDockHeight(main: RefObject<HTMLDivElement | null>) {
  useEffect(() => {
    const el = main.current;
    if (el === null || typeof ResizeObserver === 'undefined') return undefined;
    if (typeof MutationObserver === 'undefined') return undefined;
    let dock: Element | null = null;
    const sizes = new ResizeObserver(() => {
      if (dock === null) return;
      el.style.setProperty('--fd-dock-h', `${dock.getBoundingClientRect().height}px`);
    });
    const find = () => {
      const next = el.querySelector(':scope > .fd-tdock');
      if (next === dock) return;
      if (dock !== null) sizes.unobserve(dock);
      dock = next;
      if (dock === null) el.style.removeProperty('--fd-dock-h');
      else sizes.observe(dock);
    };
    find();
    const children = new MutationObserver(find);
    children.observe(el, { childList: true });
    return () => {
      children.disconnect();
      sizes.disconnect();
    };
  }, [main]);
}

/** The connection pill (§4.2): a dot and its word (Live, Stale, Offline); never color alone. */
export function LivePill({ chrome }: { readonly chrome: Chrome | undefined }) {
  return (
    <span
      className="fd-wspill"
      data-freshness={chrome?.freshness ?? 'none'}
      title={chrome?.label ?? 'Not connected'}
    >
      <span className="fd-wspill-dot" aria-hidden="true" />
      {chrome === undefined ? 'Offline' : chrome.freshness === 'live' ? 'Live' : 'Stale'}
      <span className="fd-vh">: {chrome?.label ?? 'Not connected'}</span>
    </span>
  );
}

/** One or two initials of a login actor (`owner`, `member:<name>`, `token:<name>`). */
export function initials(actor: string): string {
  const name = actor.slice(actor.indexOf(':') + 1);
  const parts = name.split(/[^\p{L}\p{N}]+/u).filter((part) => part !== '');
  const letters = parts
    .slice(0, 2)
    .map((part) => part.charAt(0))
    .join('');
  return letters === '' ? '?' : letters.toUpperCase();
}

/** The account avatar (§4.2, §4.8): a 24px circle with the initials of this login. */
export function Avatar() {
  const self = usePeople().self;
  return (
    <span className="fd-avatar" aria-hidden="true">
      {initials(self)}
    </span>
  );
}

export function Notices({ notices }: { readonly notices: readonly Notice[] }) {
  if (notices.length === 0) return null;
  return (
    <div className="fd-notices">
      {notices.map((n) => (
        <p
          key={`${n.tone}:${n.text}`}
          className="fd-notice"
          data-tone={n.tone}
          role={n.tone === 'error' ? 'alert' : 'status'}
        >
          {n.text}
        </p>
      ))}
    </div>
  );
}

/** One error banner for each session that ended before it started (R7.1). */
function StartFailures({
  failures,
  onOpen,
  onDismiss,
}: {
  readonly failures: readonly StartFailure[];
  readonly onOpen: ((failure: StartFailure) => void) | undefined;
  readonly onDismiss: ((failure: StartFailure) => void) | undefined;
}) {
  if (failures.length === 0) return null;
  return (
    <div className="fd-notices">
      {failures.map((f) => (
        <div key={f.ref} className="fd-notice fd-notice-row" data-tone="error" role="alert">
          <span className="fd-notice-text">{failureText(f)}</span>
          <button type="button" className="fd-ghost fd-sm" onClick={() => onOpen?.(f)}>
            Open
          </button>
          <button
            type="button"
            className="fd-iconbtn fd-sm"
            aria-label={`Dismiss: session ${f.title} did not start`}
            onClick={() => onDismiss?.(f)}
          >
            <Icon of={X} size={14} />
          </button>
        </div>
      ))}
    </div>
  );
}

/** Routes of the phone that show the bottom bar. */
const BAR_ROUTES: readonly Route['name'][] = [
  'home',
  'fleet',
  'new',
  'plans',
  'stations',
  'files',
  'watch',
  'activity',
  'more',
  'shares',
  'spaces',
  'artifacts',
  'artifact',
  'settings',
  'automations',
  'projects',
  'project',
  'compare',
];

/** The routes that the More tab of the phone owns (they open from the More page). */
const MORE_TAB: readonly Route['name'][] = [
  'more',
  'plans',
  'stations',
  'files',
  'watch',
  'activity',
  'shares',
  'spaces',
  'artifacts',
  'artifact',
  'settings',
  'automations',
  'projects',
  'project',
  'compare',
];

/** The page name of each route in the phone top bar (§4.8). */
const PHONE_TITLES: Partial<Record<Route['name'], string>> = {
  home: 'Sessions',
  fleet: 'Fleet',
  new: 'New session',
  more: 'More',
  plans: 'Plans',
  stations: 'Stations',
  files: 'Files',
  watch: 'Watch',
  activity: 'Activity',
  shares: 'Shares',
  spaces: 'Spaces',
  artifacts: 'Artifacts',
  artifact: 'Artifact',
  settings: 'Settings',
  automations: 'Automations',
  projects: 'Projects',
  project: 'Project',
  compare: 'Compare',
};

/**
 * The phone top bar (§4.8, 52px): the mark (or Back to More on a page of the More tab), the page
 * name, the connection and the avatar. The page keeps its own heading for screen readers.
 */
function PhoneBar({
  route,
  chrome,
}: {
  readonly route: Route;
  readonly chrome: Chrome | undefined;
}) {
  const back = route.name !== 'more' && MORE_TAB.includes(route.name);
  return (
    <header className="fd-phonebar">
      {back ? (
        <a href="#/more" className="fd-iconbtn fd-phonebar-back" aria-label="Back to More">
          <Icon of={ChevronLeft} />
        </a>
      ) : (
        <Mark />
      )}
      <span className="fd-phonebar-title fd-wide" aria-hidden="true">
        {PHONE_TITLES[route.name]}
      </span>
      <LivePill chrome={chrome} />
      <Avatar />
    </header>
  );
}

function BottomBar({ route, guest }: { readonly route: Route; readonly guest: boolean }) {
  const all: readonly {
    href: string;
    label: string;
    icon: LucideIcon;
    on: readonly Route['name'][];
    guest?: false;
  }[] = [
    { href: '#/', label: 'Sessions', icon: Layers, on: ['home'] },
    { href: '#/fleet', label: 'Fleet', icon: LayoutGrid, on: ['fleet'] },
    { href: '#/new', label: 'New', icon: Plus, on: ['new'], guest: false },
    {
      href: '#/more',
      label: 'More',
      icon: Ellipsis,
      on: MORE_TAB,
    },
  ];
  const items = guest ? all.filter((item) => item.guest !== false) : all;
  return (
    <nav className="fd-bottombar" aria-label="Board">
      {items.map((item) =>
        // §4.8: New is a 44px ink circle in the middle of the bar; its name stays "New".
        item.href === '#/new' ? (
          <a
            key={item.href}
            href={item.href}
            className="fd-bottombar-new"
            aria-current={item.on.includes(route.name) ? 'page' : undefined}
          >
            <span className="fd-bottombar-circle">
              <Icon of={item.icon} size={22} />
            </span>
            <span className="fd-vh">{item.label}</span>
          </a>
        ) : (
          <a
            key={item.href}
            href={item.href}
            aria-current={item.on.includes(route.name) ? 'page' : undefined}
          >
            <Icon of={item.icon} size={20} />
            <span>{item.label}</span>
          </a>
        ),
      )}
    </nav>
  );
}

function Drawer({
  onClose,
  children,
}: {
  readonly onClose: () => void;
  readonly children: ReactNode;
}) {
  const root = useRef<HTMLDivElement>(null);
  useFocusReturn(root);
  useOverlay();
  return (
    <div className="fd-drawer-layer">
      <button
        type="button"
        className="fd-drawer-backdrop"
        aria-label="Close sessions"
        tabIndex={-1}
        onClick={onClose}
      />
      <div
        ref={root}
        id="fd-drawer"
        className="fd-drawer"
        role="dialog"
        aria-modal="true"
        aria-label="Sessions"
        onKeyDown={(e) => {
          if (e.key === 'Escape') {
            e.stopPropagation();
            onClose();
          } else trapTab(e, root.current);
        }}
      >
        {children}
      </div>
    </div>
  );
}

/**
 * The page frame (DL-140 §4): desktop = pinned sidebar + route + dock; tablet = top bar with a
 * sessions drawer; phone = one column with a bottom bar (not on the session and terminals routes).
 */
export function Layout({
  layout,
  route,
  sidebar,
  drawerOpen,
  onDrawer,
  notices,
  banner,
  failures = [],
  onOpenFailure,
  onDismissFailure,
  dock,
  onTerminalsBack,
  overlays,
  chrome,
  children,
}: Props) {
  const showBar = layout === 'phone' && BAR_ROUTES.includes(route.name);
  const main = useRef<HTMLDivElement>(null);
  useDockHeight(main);
  // R11.4: a guest of the owner's deck sees the banner and a smaller board.
  const people = usePeople();
  return (
    <div
      className="fd-app"
      data-layout={layout}
      data-route={route.name}
      data-phonebar={showBar ? 'true' : undefined}
    >
      {layout === 'desktop' ? sidebar : null}
      {layout === 'tablet' ? (
        <header className="fd-topbar">
          <button
            type="button"
            className="fd-iconbtn"
            aria-label="Sessions"
            aria-expanded={drawerOpen}
            aria-controls="fd-drawer"
            onClick={() => onDrawer(!drawerOpen)}
          >
            <Icon of={PanelLeft} />
          </button>
          <Mark />
          <span className="fd-wordmark">Fleet Deck</span>
        </header>
      ) : null}
      <div className="fd-main" ref={main}>
        {showBar ? <PhoneBar route={route} chrome={chrome} /> : null}
        {layout === 'phone' && route.name === 'terminals' ? (
          <header className="fd-topbar">
            <button
              type="button"
              className="fd-iconbtn"
              aria-label="Back"
              onClick={onTerminalsBack}
            >
              <Icon of={ChevronLeft} />
            </button>
            <h1 className="fd-topbar-title">Terminals</h1>
          </header>
        ) : null}
        {people.member === null ? null : (
          <MemberBanner member={people.member} onLeave={people.leave} />
        )}
        <Notices notices={notices} />
        {banner}
        <StartFailures failures={failures} onOpen={onOpenFailure} onDismiss={onDismissFailure} />
        <div className="fd-route">{children}</div>
        {layout === 'phone' ? null : dock}
      </div>
      {showBar ? <BottomBar route={route} guest={people.member !== null} /> : null}
      {layout === 'tablet' && drawerOpen ? (
        <Drawer onClose={() => onDrawer(false)}>{sidebar}</Drawer>
      ) : null}
      {overlays}
    </div>
  );
}
