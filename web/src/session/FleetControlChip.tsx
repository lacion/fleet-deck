// ADR-008 §5.3 "Owner visibility": the "Fleet control" chip on the coordinator's session page. It
// shows the answers of the last hour; a click opens the list of the answers.
import { useEffect, useState } from 'react';
import type { SessionActions } from './commands';
import { type FleetAnswers, chipText } from './fleetAnswers';
import { FleetAnswersDialog } from './FleetAnswersDialog';

/** The deck is asked again after this time (ms). */
const REFRESH_MS = 60_000;

type ButtonProps = {
  readonly data: FleetAnswers;
  readonly onOpen: () => void;
};

/** The chip itself; nothing when the session has no fleet-control grant. */
export function FleetChipButton({ data, onOpen }: ButtonProps) {
  if (!data.fleet_control) return null;
  return (
    <button type="button" className="fd-chip fd-fleet-chip" onClick={onOpen}>
      {chipText(data.last_hour)}
    </button>
  );
}

/** Loads the answers on mount and every 60 s. Nothing while loading, and nothing on an error. */
export function FleetControlChip({ actions }: { actions: SessionActions }) {
  const [data, setData] = useState<FleetAnswers | null>(null);
  const [open, setOpen] = useState(false);
  useEffect(() => {
    let alive = true;
    const load = () => {
      actions.fleetAnswers().then(
        (next) => {
          if (alive) setData(next);
        },
        () => {
          if (alive) setData(null);
        },
      );
    };
    load();
    const timer = setInterval(load, REFRESH_MS);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [actions]);
  if (data === null || !data.fleet_control) return null;
  return (
    <>
      <FleetChipButton data={data} onOpen={() => setOpen(true)} />
      {open ? <FleetAnswersDialog data={data} onClose={() => setOpen(false)} /> : null}
    </>
  );
}
