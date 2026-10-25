// R13 P5b task W1 (design `omarchy-p5b.md` §12, §16): the recording part of the Desktop panel.
// `useRecording` keeps the flow of one station (see `recordFlow.ts`) and returns the pieces that
// `DesktopPanel` places: `controls(monitor)` (the duration, Record and the permanent disclosure),
// one reason line, and `block` (the live phase, Stop recording, Check result and the result).
// A recording needs no confirmation (§3.2).
import { type ReactNode, useEffect, useId, useMemo, useState, useSyncExternalStore } from 'react';
import type { DesktopInfo, StationView } from '../gen';
import { useSettings } from '../settings/context';
import { IDLE, isActive, type RecState, RecordFlow, recordingId } from './recordFlow';
import {
  availability,
  DEFAULT_SECONDS,
  DELETED_TEXT,
  deckRecords,
  DISCLOSURE,
  DURATION_LABEL,
  durationError,
  maxSeconds,
  OFFLINE_TEXT,
  PHASE_TEXT,
  parseSeconds,
  SAVED_TEXT,
  startOff,
  UNKNOWN_TEXT,
} from './recording';

/** The Record control of one monitor: the duration, the button and the disclosure. */
export function RecordPanel({
  monitor,
  max,
  off,
  onStart,
}: {
  readonly monitor: string;
  /** The longest recording of the station (`maxSeconds`). */
  readonly max: number;
  /** Record is off: not available, or another action or capture runs. */
  readonly off: boolean;
  readonly onStart: (monitor: string, seconds: number) => void;
}) {
  const [text, setText] = useState(DEFAULT_SECONDS);
  const [error, setError] = useState<string | null>(null);
  const id = useId();
  const noteId = `${id}-note`;
  const press = () => {
    const seconds = parseSeconds(text.trim(), max);
    if (seconds === null) {
      setError(durationError(max));
      return;
    }
    setError(null);
    onStart(monitor, seconds);
  };
  return (
    <div className="fd-desk-record" data-monitor={monitor} style={{ flexBasis: '100%' }}>
      <div className="fd-desk-buttons">
        <label htmlFor={`${id}-seconds`}>{DURATION_LABEL}</label>
        <input
          id={`${id}-seconds`}
          type="number"
          min={1}
          max={max}
          step={1}
          inputMode="numeric"
          required
          value={text}
          aria-invalid={error !== null}
          aria-describedby={noteId}
          onChange={(e) => {
            setText(e.target.value);
            setError(null);
          }}
        />
        <button type="button" className="fd-sm" disabled={off} onClick={press}>
          Record
        </button>
      </div>
      {error === null ? null : (
        <p role="alert" className="fd-fielderror">
          {error}
        </p>
      )}
      <p id={noteId} className="fd-note">
        {DISCLOSURE}
      </p>
    </div>
  );
}

/** The live phase, the owner stop, the unknown choice, the failure or the result. */
export function RecordBlock({
  state,
  onStop,
  onCheck,
}: {
  readonly state: RecState;
  readonly onStop: () => void;
  readonly onCheck: () => void;
}): ReactNode {
  // "Stop recording" is on whenever an ID is known; the shared busy state never turns it off.
  const stop =
    recordingId(state) === null ? null : (
      <button type="button" className="fd-ghost" onClick={onStop}>
        Stop recording
      </button>
    );
  const error =
    'error' in state && state.error !== null ? (
      <p className="fd-fielderror" role="alert">
        {state.error}
      </p>
    ) : null;
  switch (state.kind) {
    case 'idle':
    case 'discovering':
      return null;
    case 'running':
      return (
        <div className="fd-desk-result">
          <p role="status">{PHASE_TEXT[state.phase ?? 'preparing']}</p>
          {stop}
          {error}
        </div>
      );
    case 'expired':
      return (
        <div className="fd-disp-confirm" role="alert">
          <p>{UNKNOWN_TEXT}</p>
          <button type="button" className="fd-ghost" disabled={state.checking} onClick={onCheck}>
            Check result
          </button>
          {stop}
          {error}
        </div>
      );
    case 'unreachable':
      return (
        <div className="fd-desk-result">
          <p role="alert" className="fd-fielderror">
            {OFFLINE_TEXT}
          </p>
          {stop}
          {error}
        </div>
      );
    case 'failed':
      return (
        <p className="fd-fielderror" role="alert">
          {state.text}
        </p>
      );
    case 'deleted':
      return <p role="status">{DELETED_TEXT}</p>;
    case 'committed': {
      const { artifact } = state.published;
      return (
        <div className="fd-desk-result">
          {state.after202 ? <p role="status">{SAVED_TEXT}</p> : null}
          <a
            href={`#/artifact/${encodeURIComponent(artifact.station_id)}/${encodeURIComponent(artifact.artifact_id)}`}
          >
            Open the recording
          </a>
        </div>
      );
    }
  }
}

const NEVER = () => () => {};
const IDLE_SNAPSHOT = () => IDLE;

const storageOrNull = (): Storage | null => {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
};

export type RecordUi = {
  /** A recording runs on the station (or is being looked up): Screenshot is off meanwhile. */
  readonly active: boolean;
  /** The Record control of one monitor; `slotActive`: a screenshot runs on the station. */
  readonly controls: (monitorName: string, slotActive?: boolean) => ReactNode;
  readonly reason: string | null;
  readonly block: ReactNode;
};

/**
 * The recording flow of one station for `DesktopPanel`. Call it once, before any early return.
 * `deck` is the `/api/features` list; `busy` is true while another action is running.
 */
export function useRecording(
  station: StationView,
  info: DesktopInfo | undefined,
  busy: boolean,
  deck: readonly string[] | null | undefined,
): RecordUi {
  const env = useSettings();
  const capture = env?.capture;
  const flow = useMemo(
    () =>
      capture === undefined
        ? null
        : new RecordFlow({
            stationName: station.name,
            stationId: station.station_id,
            actions: capture,
            storage: storageOrNull(),
          }),
    [capture, station.name, station.station_id],
  );
  // The deck is asked for an active recording only when it knows recordings; Stop works then
  // whatever the capture setting of the station says.
  const known = deckRecords(deck);
  useEffect(() => {
    if (!known) return undefined;
    flow?.attach();
    return () => flow?.detach();
  }, [flow, known]);
  const state = useSyncExternalStore(
    flow === null ? NEVER : flow.subscribe,
    flow === null ? IDLE_SNAPSHOT : flow.getSnapshot,
    IDLE_SNAPSHOT,
  );
  const active = isActive(state);
  const max = maxSeconds(info);
  const reasonOf = (monitor: string) =>
    info === undefined ? { enabled: false, reason: null } : availability(deck, info, monitor);
  const reason =
    info === undefined
      ? null
      : (info.monitors.map((m) => reasonOf(m.name).reason).find((r) => r !== null) ?? null);
  return {
    active,
    reason,
    controls: (monitorName, slotActive = false) => (
      <RecordPanel
        monitor={monitorName}
        max={max}
        off={startOff({
          busy: busy || flow === null,
          slotActive: slotActive || active,
          enabled: reasonOf(monitorName).enabled,
        })}
        onStart={(monitor, seconds) => void flow?.start(monitor, seconds, max)}
      />
    ),
    block:
      flow === null ? null : (
        <RecordBlock
          state={state}
          onStop={() => void flow.stop()}
          onCheck={() => void flow.check()}
        />
      ),
  };
}
