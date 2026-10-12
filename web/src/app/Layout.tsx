import { type ReactNode, useRef } from 'react';
import type { Notice } from './chrome';
import { trapTab, useFocusReturn } from './focus';
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

/** Routes of the phone that show the bottom bar. */
const BAR_ROUTES: readonly Route['name'][] = ['home', 'fleet', 'new', 'plans', 'stations', 'more'];

function BottomBar({ route }: { readonly route: Route }) {
  const items: readonly { href: string; label: string; on: readonly Route['name'][] }[] = [
    { href: '#/', label: 'Sessions', on: ['home'] },
    { href: '#/fleet', label: 'Fleet', on: ['fleet'] },
    { href: '#/new', label: 'New', on: ['new'] },
    { href: '#/more', label: 'More', on: ['more', 'plans', 'stations'] },
  ];
  return (
    <nav className="fd-bottombar" aria-label="Board">
      {items.map((item) => (
        <a
          key={item.href}
          href={item.href}
          aria-current={item.on.includes(route.name) ? 'page' : undefined}
        >
          {item.label}
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
  dock,
  onTerminalsBack,
  overlays,
  children,
}: Props) {
  const showBar = layout === 'phone' && BAR_ROUTES.includes(route.name);
  return (
    <div className="fd-app" data-layout={layout} data-route={route.name}>
      {layout === 'desktop' ? sidebar : null}
      {layout === 'tablet' ? (
        <header className="fd-topbar">
          <button
            type="button"
            aria-expanded={drawerOpen}
            aria-controls="fd-drawer"
            onClick={() => onDrawer(!drawerOpen)}
          >
            Sessions
          </button>
          <span className="fd-wordmark">FLEET DECK</span>
        </header>
      ) : null}
      <div className="fd-main">
        {layout === 'phone' && route.name === 'terminals' ? (
          <header className="fd-topbar">
            <button type="button" aria-label="Back" onClick={onTerminalsBack}>
              Back
            </button>
            <h1 className="fd-topbar-title">Terminals</h1>
          </header>
        ) : null}
        <Notices notices={notices} />
        <div className="fd-route">{children}</div>
        {layout === 'phone' ? null : dock}
      </div>
      {showBar ? <BottomBar route={route} /> : null}
      {layout === 'tablet' && drawerOpen ? (
        <Drawer onClose={() => onDrawer(false)}>{sidebar}</Drawer>
      ) : null}
      {overlays}
    </div>
  );
}
