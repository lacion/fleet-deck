// The tabbed part of the fleet home's ops panel (DL-141, DESIGN-SYSTEM §4.4): Activity, Mail &
// tasks, Assign and Usage. On a wide screen it is the foot of the right ops column and opens
// upward; under 1280 px and on the phone it folds under the board.
import { useEffect, useRef, useState } from 'react';
import type { AssignActions } from '../assign/assign';
import { AssignBar } from '../assign/AssignBar';
import type { FleetActions } from '../fleet/fleet';
import { FleetPanel } from '../fleet/FleetPanel';
import type { DeckSession, SessionKey, StationView } from '../gen';
import { ChevronDown, ChevronUp, Icon } from '../ui/icons';
import type { FeedItem } from './feed';
import { Feed } from './FeedPanel';
import { UsageMeters } from './UsageMeters';

export type PanelTab = 'activity' | 'mail' | 'assign' | 'usage';
type PanelState = { readonly open: boolean; readonly tab: PanelTab };
const PANEL_KEY = 'fd-home-panel';
const PANEL_DEFAULT: PanelState = { open: false, tab: 'activity' };
const PANEL_TABS: readonly PanelTab[] = ['activity', 'mail', 'assign', 'usage'];

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

/** The tabs of the ops panel, and the body of the open tab. */
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
}) {
  const [state, setState] = useState<PanelState>(() => readPanel(hasAssign));
  const seen = useRef<number | null>(null);
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
  }, [seq, asked, hasAssign]);
  const set = (next: PanelState) => {
    setState(next);
    writePanel(next);
  };
  const tab: PanelTab =
    (state.tab === 'assign' && !hasAssign) || (guest && state.tab === 'mail')
      ? 'activity'
      : state.tab;
  const shown = state.open ? tab : null;
  useEffect(() => onState?.(shown), [onState, shown]);
  const tabs = PANEL_TABS.filter((t) => (t !== 'assign' || hasAssign) && !(guest && t === 'mail'));
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
