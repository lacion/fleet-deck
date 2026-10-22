// ADR-008 §5.3 "Owner visibility": the fleet-control fact of a coordinator's session. It is a fact
// of the session, not a row of its own between the header and the tabs: a "Fleet control" row of
// the Properties panel, or of the details of the session menu when the panel is not open. Its value
// is a link with the answers of the last hour; a press opens the list of the answers.
import { type ReactNode, useEffect, useState } from 'react';
import type { SessionActions } from './commands';
import { type FleetAnswers, answersText } from './fleetAnswers';
import { FleetAnswersDialog } from './FleetAnswersDialog';

/** The deck is asked again after this time (ms). */
const REFRESH_MS = 60_000;

type ButtonProps = {
  readonly data: FleetAnswers;
  readonly onOpen: () => void;
};

/** The value of the fact: a link button; nothing when the session has no fleet-control grant. */
export function FleetChipButton({ data, onOpen }: ButtonProps) {
  if (!data.fleet_control) return null;
  return (
    <button type="button" className="fd-link fd-props-link fd-fleet-link" onClick={onOpen}>
      {answersText(data.last_hour)}
    </button>
  );
}

/** The fleet-control fact of a session and the dialog of its answers. */
export type FleetControl = {
  /** The link; `before` runs first (it closes the menu that holds the link). `null`: no fact. */
  readonly link: ((before?: () => void) => ReactNode) | null;
  /** The open dialog of the answers, or `null`. */
  readonly dialog: ReactNode;
};

/**
 * Loads the answers on mount and every 60 s; `null` actions load nothing. No fact while loading,
 * on an error, or without a fleet-control grant.
 */
export function useFleetControl(actions: SessionActions | null): FleetControl {
  const [data, setData] = useState<FleetAnswers | null>(null);
  const [open, setOpen] = useState(false);
  useEffect(() => {
    if (actions === null) {
      setData(null);
      return undefined;
    }
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
  const shown = data?.fleet_control === true ? data : null;
  return {
    link:
      shown === null
        ? null
        : (before) => (
            <FleetChipButton
              data={shown}
              onOpen={() => {
                before?.();
                setOpen(true);
              }}
            />
          ),
    dialog:
      open && shown !== null ? (
        <FleetAnswersDialog data={shown} onClose={() => setOpen(false)} />
      ) : null,
  };
}
