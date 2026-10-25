import { type KeyboardEvent, type PointerEvent, useRef, useState } from 'react';
import type { ShellActions } from '../shell/shell';
import {
  ChevronDown,
  ChevronUp,
  Ellipsis,
  Icon,
  MessageSquare,
  Plus,
  SquareTerminal,
  Undo2,
  X,
} from '../ui/icons';
import { Menu } from '../ui/Menu';
import { Terminal } from './Terminal';
import { MIN_HEIGHT, type TermTab, type TerminalStore, useDock } from './store';

type Props = {
  readonly store: TerminalStore;
  readonly actions: ShellActions;
  readonly stationName: (id: string) => string;
  readonly layout: 'dock' | 'full';
  readonly touch: boolean;
  /** "Return to board control" of a TUI tab. */
  readonly onReturn: (tab: TermTab) => void;
  /** Given: the tab bar offers "Open shell in workspace". */
  readonly onNewShell?: () => void;
};

const STEP = 24;
const message = (e: unknown) => (e instanceof Error ? e.message : 'Request failed');

/** The tallest the dock may be: 70% of the window, never below the minimum. */
const maxHeight = (): number =>
  typeof window === 'undefined' ? 640 : Math.max(MIN_HEIGHT, Math.floor(window.innerHeight * 0.7));

/**
 * The terminals of the board (DL-140): shell and TUI tabs that live in a store, so that
 * navigation and a hidden dock never close a shell. Every terminal stays mounted.
 */
export function TerminalDock({
  store,
  actions,
  stationName,
  layout,
  touch,
  onReturn,
  onNewShell,
}: Props) {
  const dock = useDock(store);
  const [confirming, setConfirming] = useState<string | null>(null);
  const [failure, setFailure] = useState<{ tab: string; text: string } | null>(null);
  const tabRefs = useRef(new Map<string, HTMLButtonElement>());
  const drag = useRef<{ y: number; height: number } | null>(null);

  if (dock.tabs.length === 0) return null;

  const current = dock.tabs.find((t) => t.id === dock.active) ?? dock.tabs[0] ?? null;
  const visible = dock.open && dock.tabs.length > 0;

  const moveFocus = (e: KeyboardEvent, index: number) => {
    const last = dock.tabs.length - 1;
    let to: number;
    if (e.key === 'ArrowRight') to = index === last ? 0 : index + 1;
    else if (e.key === 'ArrowLeft') to = index === 0 ? last : index - 1;
    else if (e.key === 'Home') to = 0;
    else if (e.key === 'End') to = last;
    else return;
    e.preventDefault();
    const next = dock.tabs[to];
    if (next === undefined) return;
    store.activate(next.id);
    tabRefs.current.get(next.id)?.focus();
  };

  const closeShell = async (tab: TermTab): Promise<boolean> => {
    try {
      await actions.close(tab.station, tab.shellId);
      setConfirming(null);
      setFailure(null);
      store.remove(tab.id);
      return true;
    } catch (e) {
      setFailure({ tab: tab.id, text: `Not closed: ${message(e)}` });
      return false;
    }
  };

  const onDragDown = (e: PointerEvent<HTMLDivElement>) => {
    drag.current = { y: e.clientY, height: dock.height };
    e.currentTarget.setPointerCapture?.(e.pointerId);
  };
  const onDragMove = (e: PointerEvent<HTMLDivElement>) => {
    const start = drag.current;
    if (start === null) return;
    store.setHeight(start.height + (start.y - e.clientY), maxHeight());
  };
  const onDragEnd = () => {
    drag.current = null;
  };
  const onHandleKey = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === 'ArrowUp') store.setHeight(dock.height + STEP, maxHeight());
    else if (e.key === 'ArrowDown') store.setHeight(dock.height - STEP, maxHeight());
    else return;
    e.preventDefault();
  };

  const panelStyle = layout === 'dock' && visible ? { height: dock.height } : undefined;
  return (
    <section
      className="fd-tdock"
      aria-label="Terminals"
      data-layout={layout}
      data-open={visible ? 'true' : 'false'}
      style={panelStyle}
    >
      {layout === 'dock' && visible ? (
        <div
          className="fd-tdock-handle"
          role="separator"
          aria-orientation="horizontal"
          aria-label="Resize terminals"
          aria-valuemin={MIN_HEIGHT}
          aria-valuemax={maxHeight()}
          aria-valuenow={dock.height}
          tabIndex={0}
          onPointerDown={onDragDown}
          onPointerMove={onDragMove}
          onPointerUp={onDragEnd}
          onPointerCancel={onDragEnd}
          onKeyDown={onHandleKey}
        />
      ) : null}
      <div className="fd-tdock-bar">
        <div className="fd-tdock-tabs" role="tablist" aria-label="Open terminals">
          {dock.tabs.map((tab, index) => {
            const selected = tab.id === current?.id;
            return (
              <div className="fd-tdock-tabwrap" key={tab.id} data-selected={selected}>
                <button
                  type="button"
                  role="tab"
                  id={`fd-tab-${tab.id}`}
                  aria-selected={selected}
                  aria-controls={`fd-pane-${tab.id}`}
                  tabIndex={selected ? 0 : -1}
                  className="fd-tdock-tab"
                  ref={(el) => {
                    if (el === null) tabRefs.current.delete(tab.id);
                    else tabRefs.current.set(tab.id, el);
                  }}
                  onClick={() => store.activate(tab.id)}
                  onKeyDown={(e) => moveFocus(e, index)}
                >
                  <Icon of={tab.kind === 'shell' ? SquareTerminal : MessageSquare} size={14} />
                  <span className="fd-tdock-title">{tab.title}</span>
                  <span className="fd-tdock-station">{stationName(tab.station)}</span>
                </button>
                <button
                  type="button"
                  className="fd-iconbtn fd-sm fd-tdock-detach"
                  aria-label={`Detach ${tab.title}`}
                  tabIndex={selected ? 0 : -1}
                  onClick={() => store.remove(tab.id)}
                >
                  <Icon of={X} size={14} />
                </button>
              </div>
            );
          })}
        </div>
        <div className="fd-tdock-tools">
          {onNewShell === undefined ? null : (
            <button
              type="button"
              className="fd-iconbtn"
              aria-label="Open shell in workspace"
              title="Open shell in workspace"
              onClick={onNewShell}
            >
              <Icon of={Plus} />
            </button>
          )}
          {current === null ? null : current.kind === 'tui' ? (
            <button type="button" className="fd-sm" onClick={() => onReturn(current)}>
              <Icon of={Undo2} size={14} /> Return to board control
            </button>
          ) : (
            <Menu
              summary={<Icon of={Ellipsis} />}
              summaryClass="fd-iconbtn"
              label="Terminal menu"
              title="Terminal menu"
              onClosed={() => setConfirming(null)}
            >
              {(close) =>
                confirming === current.id ? (
                  <>
                    <p className="fd-menu-text">Close the shell? Its process ends.</p>
                    <button
                      type="button"
                      className="fd-danger"
                      onClick={() =>
                        void closeShell(current).then((closed) => {
                          if (closed) close();
                        })
                      }
                    >
                      Close shell
                    </button>
                    <button
                      type="button"
                      onClick={() => {
                        setConfirming(null);
                        setFailure(null);
                      }}
                    >
                      Cancel
                    </button>
                  </>
                ) : (
                  <button
                    type="button"
                    className="fd-danger"
                    onClick={() => {
                      setFailure(null);
                      setConfirming(current.id);
                    }}
                  >
                    Close shell
                  </button>
                )
              }
            </Menu>
          )}
          <button
            type="button"
            className="fd-iconbtn"
            aria-expanded={dock.open}
            aria-label={dock.open ? 'Hide terminals' : 'Show terminals'}
            title={dock.open ? 'Hide terminals' : 'Show terminals'}
            onClick={() => store.setOpen(!dock.open)}
          >
            <Icon of={dock.open ? ChevronDown : ChevronUp} />
          </button>
        </div>
      </div>
      {current !== null && failure !== null && failure.tab === current.id ? (
        <p className="fd-tdock-error" role="alert">
          {failure.text}
        </p>
      ) : null}
      <div className="fd-tdock-body" hidden={!visible}>
        {dock.tabs.map((tab) => {
          const selected = tab.id === current?.id;
          return (
            <div
              className="fd-tdock-pane"
              key={tab.id}
              id={`fd-pane-${tab.id}`}
              role="tabpanel"
              aria-labelledby={`fd-tab-${tab.id}`}
              hidden={!selected}
            >
              <Terminal
                station={tab.station}
                shellId={tab.shellId}
                kind={tab.kind}
                actions={actions}
                active={selected && visible}
                touch={touch}
                onEnded={() => store.remove(tab.id)}
              />
            </div>
          );
        })}
      </div>
    </section>
  );
}
