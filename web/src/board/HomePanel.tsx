// The secondary part of the fleet home's ops panel (DL-141, DESIGN-SYSTEM §4.4): Activity, Mail &
// tasks, Assign and Usage. From 1280 px (the wide ops column) Mail & tasks is an always-visible
// section, and Activity, Assign and Usage are ghost buttons in a foot row that open their panel above
// it. Under 1280 px and on the phone the panel folds under the board as tabs.
import { useEffect, useRef, useState, useSyncExternalStore } from 'react';
import type { AssignActions } from '../assign/assign';
import { AssignBar } from '../assign/AssignBar';
import type { FleetActions } from '../fleet/fleet';
import { FleetPanel, type MailWaiting } from '../fleet/FleetPanel';
import type { DeckSession, SessionKey, StationView } from '../gen';
import {
  Activity,
  ChevronDown,
  ChevronUp,
  Fuel,
  Icon,
  type LucideIcon,
  Mail,
  Send,
} from '../ui/icons';
import type { FeedItem } from './feed';
import { Feed } from './FeedPanel';
import { UsageMeters } from './UsageMeters';

export type PanelTab = 'activity' | 'mail' | 'assign' | 'usage';
type PanelState = { readonly open: boolean; readonly tab: PanelTab };
const PANEL_KEY = 'fd-home-panel';
const PANEL_DEFAULT: PanelState = { open: false, tab: 'activity' };
const PANEL_TABS: readonly PanelTab[] = ['activity', 'mail', 'assign', 'usage'];

/** The width from which the ops panel is the right column (home.css uses the same query). */
const WIDE = '(min-width: 1280px)';

function subscribeWide(cb: () => void): () => void {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return () => {};
  const list = window.matchMedia(WIDE);
  list.addEventListener('change', cb);
  return () => list.removeEventListener('change', cb);
}

const wideNow = () =>
  typeof window !== 'undefined' &&
  typeof window.matchMedia === 'function' &&
  window.matchMedia(WIDE).matches;

/** True from 1280 px: the ops panel is the right column. Without a window: false (the tabs). */
export function useWideOps(): boolean {
  return useSyncExternalStore(subscribeWide, wideNow, () => false);
}

function readPanel(hasAssign: boolean): PanelState {
  try {
    const raw = localStorage.getItem(PANEL_KEY);
    if (raw === null) return PANEL_DEFAULT;
    const v: unknown = JSON.parse(raw);
    if (typeof v !== 'object' || v === null) return PANEL_DEFAULT;
    const { open, tab } = v as { open?: unknown; tab?: unknown };
    if (typeof open !== 'boolean' || !PANEL_TABS.includes(tab as PanelTab)) return PANEL_DEFAULT;
    if (tab === 'assign' && !hasAssign) return PANEL_DEFAULT;
    return { open, tab: tab as PanelTab };
  } catch {
    return PANEL_DEFAULT;
  }
}

function writePanel(state: PanelState) {
  try {
    localStorage.setItem(PANEL_KEY, JSON.stringify(state));
  } catch {
    // Storage is blocked: the panel still works for this visit.
  }
}

const PANEL_LABEL: Record<PanelTab, string> = {
  activity: 'Activity',
  mail: 'Mail & tasks',
  assign: 'Assign',
  usage: 'Usage',
};

const PANEL_ICON: Record<PanelTab, LucideIcon> = {
  activity: Activity,
  mail: Mail,
  assign: Send,
  usage: Fuel,
};

/** The tabs of the ops panel and the body of the open tab; or, wide, the Mail & tasks section
 * and the foot buttons. */
export function HomePanel({
  hasAssign,
  guest,
  stations,
  sessions,
  visible,
  feed,
  fleet,
  assign,
  onOpen,
  onSpawn,
  request,
  onState,
  wide = false,
  waiting = [],
}: {
  hasAssign: boolean;
  /** A guest has no mail and no assign tab (the deck refuses the calls). */
  guest: boolean;
  stations: readonly StationView[];
  sessions: readonly DeckSession[];
  visible: readonly DeckSession[];
  feed: readonly FeedItem[];
  fleet: FleetActions | undefined;
  assign: AssignActions | undefined;
  onOpen: (key: SessionKey) => void;
  onSpawn: (stationId: string, text: string) => void;
  request: { readonly tab: PanelTab; readonly seq: number } | undefined;
  /** Tells the ops column which tab is open (`null`: the panel is closed). */
  onState?: ((open: PanelTab | null) => void) | undefined;
  /** The wide ops column (from 1280 px): the Mail & tasks section and the foot buttons. */
  wide?: boolean | undefined;
  /** The waiting mail rows of the Mail & tasks section (wide). */
  waiting?: readonly MailWaiting[] | undefined;
}) {
  const [state, setState] = useState<PanelState>(() => readPanel(hasAssign));
  const seen = useRef<number | null>(null);
  const mailRef = useRef<HTMLDivElement>(null);
  /** Counts the wide requests for Mail & tasks: each one opens the new-mail form (the `c` key). */
  const [composeSeq, setComposeSeq] = useState(0);
  const seq = request?.seq;
  const asked = request?.tab;
  useEffect(() => {
    if (seq === undefined || asked === undefined || seq === seen.current) return;
    seen.current = seq;
    const next: PanelState = {
      open: true,
      tab: asked === 'assign' && !hasAssign ? 'activity' : asked,
    };
    setState(next);
    writePanel(next);
    // Wide, Mail & tasks is a section of the column: open its form and bring it into view.
    if (wide && next.tab === 'mail') {
      setComposeSeq((n) => n + 1);
      mailRef.current?.scrollIntoView?.({ block: 'nearest' });
    }
  }, [seq, asked, hasAssign, wide]);
  const set = (next: PanelState) => {
    setState(next);
    writePanel(next);
  };
  const tab: PanelTab =
    (state.tab === 'assign' && !hasAssign) || (guest && state.tab === 'mail')
      ? 'activity'
      : state.tab;
  const tabs = PANEL_TABS.filter((t) => (t !== 'assign' || hasAssign) && !(guest && t === 'mail'));
  // Wide: Mail & tasks is a section. Usage stays in the foot: the glance shows only the tightest
  // meters, and the Usage panel keeps every meter and the unknown parts.
  const foot: readonly PanelTab[] = tabs.filter((t) => t !== 'mail');
  const shown = state.open && (!wide || foot.includes(tab)) ? tab : null;
  useEffect(() => onState?.(shown), [onState, shown]);
  const body = (
    <>
      {shown === 'activity' ? <Feed items={feed} onOpen={onOpen} /> : null}
      {shown === 'mail' && !guest ? (
        <FleetPanel actions={fleet} sessions={sessions} stations={stations} />
      ) : null}
      {shown === 'assign' && assign !== undefined ? (
        <AssignBar actions={assign} onSpawn={onSpawn} />
      ) : null}
      {shown === 'usage' ? <UsageMeters sessions={visible} stations={stations} /> : null}
    </>
  );
  if (wide) {
    return (
      <>
        {guest ? null : (
          <div className="fd-ops-mail" ref={mailRef}>
            <FleetPanel
              actions={fleet}
              sessions={sessions}
              stations={stations}
              layout="section"
              composeSeq={composeSeq}
              waiting={waiting}
            />
          </div>
        )}
        <section
          className="fd-homepanel"
          data-wide="true"
          data-open={shown !== null}
          aria-label="Fleet panel"
        >
          {shown === null ? null : (
            <div id="fd-homepanel-body" className="fd-homepanel-body">
              {body}
            </div>
          )}
          <div className="fd-ops-foot">
            {foot.map((t) => (
              <button
                key={t}
                type="button"
                className="fd-ghost"
                aria-expanded={shown === t}
                aria-controls={shown === t ? 'fd-homepanel-body' : undefined}
                onClick={() =>
                  set(shown === t ? { ...state, open: false } : { open: true, tab: t })
                }
              >
                <Icon of={PANEL_ICON[t]} size={16} /> {PANEL_LABEL[t]}
              </button>
            ))}
          </div>
        </section>
      </>
    );
  }
  return (
    <section className="fd-homepanel" data-open={state.open} aria-label="Fleet panel">
      <div className="fd-homepanel-head">
        <div role="tablist" aria-label="Fleet panels" className="fd-seg fd-homepanel-tabs">
          {tabs.map((t) => (
            <button
              key={t}
              type="button"
              role="tab"
              aria-selected={state.open && tab === t}
              aria-controls="fd-homepanel-body"
              onClick={() => set({ open: true, tab: t })}
            >
              {PANEL_LABEL[t]}
            </button>
          ))}
        </div>
        <button
          type="button"
          className="fd-iconbtn fd-sm"
          aria-expanded={state.open}
          aria-controls="fd-homepanel-body"
          aria-label={state.open ? 'Collapse panel' : 'Expand panel'}
          onClick={() => set({ ...state, open: !state.open })}
        >
          <Icon of={state.open ? ChevronDown : ChevronUp} />
        </button>
      </div>
      <div
        id="fd-homepanel-body"
        className="fd-homepanel-body"
        role="tabpanel"
        hidden={!state.open}
      >
        {tab === 'activity' ? <Feed items={feed} onOpen={onOpen} /> : null}
        {tab === 'mail' && !guest ? (
          <FleetPanel actions={fleet} sessions={sessions} stations={stations} />
        ) : null}
        {tab === 'assign' && assign !== undefined ? (
          <AssignBar actions={assign} onSpawn={onSpawn} />
        ) : null}
        {tab === 'usage' ? <UsageMeters sessions={visible} stations={stations} /> : null}
      </div>
    </section>
  );
}
