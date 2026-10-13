import { type ReactNode, useRef } from 'react';
import {
  ChevronLeft,
  Ellipsis,
  Icon,
  LayoutGrid,
  type LucideIcon,
  MessagesSquare,
  PanelLeft,
  Plus,
  X,
  Zap,
} from '../ui/icons';
import { usePeople } from '../sharing/context';
import { MemberBanner } from '../sharing/MemberBanner';
import type { Notice } from './chrome';
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
  readonly children: ReactNode;
};

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

function BottomBar({ route, guest }: { readonly route: Route; readonly guest: boolean }) {
  const all: readonly {
    href: string;
    label: string;
    icon: LucideIcon;
    on: readonly Route['name'][];
    guest?: false;
  }[] = [
    { href: '#/', label: 'Sessions', icon: MessagesSquare, on: ['home'] },
    { href: '#/fleet', label: 'Fleet', icon: LayoutGrid, on: ['fleet'] },
    { href: '#/new', label: 'New', icon: Plus, on: ['new'], guest: false },
    {
      href: '#/more',
      label: 'More',
      icon: Ellipsis,
      on: [
        'more',
        'plans',
        'stations',
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
      ],
    },
  ];
  const items = guest ? all.filter((item) => item.guest !== false) : all;
  return (
    <nav className="fd-bottombar" aria-label="Board">
      {items.map((item) => (
        <a
          key={item.href}
          href={item.href}
          aria-current={item.on.includes(route.name) ? 'page' : undefined}
        >
          <Icon of={item.icon} size={20} />
          <span>{item.label}</span>
        </a>
      ))}
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
  children,
}: Props) {
  const showBar = layout === 'phone' && BAR_ROUTES.includes(route.name);
  // R11.4: a guest of the owner's deck sees the banner and a smaller board.
  const people = usePeople();
  return (
    <div className="fd-app" data-layout={layout} data-route={route.name}>
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
          <Icon of={Zap} />
          <span className="fd-wordmark">FLEET DECK</span>
        </header>
      ) : null}
      <div className="fd-main">
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
