// DL-178: the tailnet flow of the Add station dialog: choose a computer, run the command on it,
// and confirm the code. The deck polls the request window, the requests and the devices.
// Nothing is added by itself: the owner compares the code and approves (with an elevation).
import { type ReactNode, useCallback, useEffect, useRef, useState } from 'react';
import { Steps } from '../board/Steps';
import type { SettingsEnv } from '../settings/context';
import { useElevation } from '../settings/elevation';
import { DeckError, describe } from '../settings/http';
import { useNow } from '../ui/useNow';
import type { DevicesView, PairRequest, RequestsView, WindowView } from './api';
import { CommandBlock, WaitLine } from './CommandBlock';
import { DeviceList } from './DeviceList';
import { approveError, countdownText, groupDevices, requestOf } from './model';
import { RequestCard } from './RequestCard';

export { RequestCard };

const REFRESH_MS = 3_000;
const OPEN_LABEL = 'Accept a new station for 10 minutes';
const NO_LISTENER_TEXT =
  'The deck has no tailnet link address yet. Use tailnet in Settings → Deck, then restart the deck.';
const STEPS = ['Choose', 'Run', 'Confirm'] as const;

type Outcome = { readonly kind: 'added' | 'denied'; readonly name: string };

function WindowChip({ win, onStop }: { readonly win: WindowView; readonly onStop: () => void }) {
  const now = useNow(undefined, 1_000);
  if (!win.open) return null;
  const left = win.deadline_ms === null ? null : countdownText(win.deadline_ms - now);
  return (
    <div className="fd-add-chip">
      <span>Accepting new stations{left === null ? '' : ` · ${left}`}</span>
      <button type="button" className="fd-add-text-btn" onClick={onStop}>
        Stop
      </button>
    </div>
  );
}

export function TailnetPairing({
  env,
  onInvite,
}: {
  readonly env: SettingsEnv;
  readonly onInvite: () => void;
}) {
  const { tailnet } = env;
  const { ask, node } = useElevation(env.settings);
  const [win, setWin] = useState<WindowView | null>(null);
  const [requests, setRequests] = useState<RequestsView['requests']>([]);
  const [devices, setDevices] = useState<DevicesView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [windowError, setWindowError] = useState<string | null>(null);
  const [requestErrors, setRequestErrors] = useState<Readonly<Record<string, string>>>({});
  const [chosen, setChosen] = useState<string | null>(null);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const [opening, setOpening] = useState(false);
  const [acting, setActing] = useState<string | null>(null);
  const calls = useRef(0);
  const alive = useRef(true);

  const refresh = useCallback(async () => {
    const call = ++calls.current;
    try {
      const [w, r, d] = await Promise.all([
        tailnet.window(),
        tailnet.requests(),
        tailnet.devices(),
      ]);
      if (!alive.current || call !== calls.current) return;
      setWin(w);
      setRequests(r.requests);
      setDevices(d);
      setError(null);
    } catch (e) {
      if (!alive.current || call !== calls.current) return;
      setError(describe(e));
    }
  }, [tailnet]);

  useEffect(() => {
    alive.current = true;
    void refresh();
    const id = setInterval(() => void refresh(), REFRESH_MS);
    return () => {
      alive.current = false;
      clearInterval(id);
    };
  }, [refresh]);

  /** A refused elevation is dropped, so that the next try asks again. */
  const forgetIfRefused = (e: unknown) => {
    if (e instanceof DeckError && e.code === 'elevation_required') env.settings.forgetElevation();
  };

  /** Opens the request window. True when it is open. */
  const openWindow = async (): Promise<boolean> => {
    setWindowError(null);
    setOpening(true);
    let opened = false;
    try {
      const elevation = await ask([OPEN_LABEL]);
      if (elevation !== null) {
        await tailnet.setWindow(true, elevation);
        opened = true;
      }
    } catch (e) {
      forgetIfRefused(e);
      if (alive.current) {
        setWindowError(
          e instanceof DeckError && e.code === 'no_tailnet_listener'
            ? NO_LISTENER_TEXT
            : describe(e),
        );
      }
    }
    await refresh();
    if (alive.current) setOpening(false);
    return opened;
  };

  const closeWindow = async () => {
    setWindowError(null);
    try {
      await tailnet.setWindow(false, null);
    } catch (e) {
      if (alive.current) setWindowError(describe(e));
    }
    await refresh();
  };

  /** "Add": the device is chosen, and a cancel or an error of the window goes back to the list. */
  const add = async (dnsName: string) => {
    setOutcome(null);
    setChosen(dnsName);
    if (win?.open !== true && !(await openWindow()) && alive.current) setChosen(null);
  };

  const fail = (id: string, text: string) => {
    if (alive.current) setRequestErrors((prev) => ({ ...prev, [id]: text }));
  };
  const clearFail = (id: string) =>
    setRequestErrors((prev) => Object.fromEntries(Object.entries(prev).filter(([k]) => k !== id)));

  const approve = async (r: PairRequest) => {
    clearFail(r.request_id);
    setActing(r.request_id);
    try {
      const elevation = await ask([`Pair station ${r.requested_name}`]);
      if (elevation === null) return;
      await tailnet.approve(r.request_id, r.station_fingerprint, r.digest, elevation);
      if (alive.current) {
        setOutcome({ kind: 'added', name: r.requested_name });
        setChosen(null);
      }
    } catch (e) {
      forgetIfRefused(e);
      fail(r.request_id, e instanceof DeckError ? approveError(e.code, e.message) : describe(e));
    } finally {
      if (alive.current) setActing(null);
    }
    await refresh();
  };

  const deny = async (r: PairRequest) => {
    clearFail(r.request_id);
    setActing(r.request_id);
    try {
      await tailnet.deny(r.request_id);
      if (alive.current) {
        setOutcome({ kind: 'denied', name: r.requested_name });
        setChosen(null);
      }
    } catch (e) {
      fail(r.request_id, describe(e));
    } finally {
      if (alive.current) setActing(null);
    }
    await refresh();
  };

  const card = (r: PairRequest) => (
    <RequestCard
      key={r.request_id}
      request={r}
      busy={acting === r.request_id}
      error={requestErrors[r.request_id] ?? null}
      onApprove={() => void approve(r)}
      onDeny={() => void deny(r)}
    />
  );

  const available = devices?.available === true;
  const list = devices?.available ? devices.devices : [];
  const device = chosen === null ? undefined : list.find((d) => d.dns_name === chosen);
  const matched = device === undefined ? undefined : requestOf(device, requests);
  const others = requests.filter((r) => r !== matched);
  const step = outcome !== null ? 2 : matched !== undefined ? 2 : device !== undefined ? 1 : 0;

  let body: ReactNode;
  if (devices === null) {
    body = <p className="fd-note">Checking Tailscale…</p>;
  } else if (!devices.available) {
    body = (
      <>
        <p className="fd-note">
          Tailscale is not available on this deck: {devices.reason ?? 'no reason given'}.
        </p>
        <button type="button" onClick={onInvite}>
          Use an invitation instead
        </button>
      </>
    );
  } else if (outcome !== null) {
    body = (
      <>
        {outcome.kind === 'added' ? (
          <p className="fd-add-done" role="status">
            ✓ {outcome.name} is added. It connects to this deck in a few seconds.
          </p>
        ) : (
          <p role="status">{outcome.name} was not added.</p>
        )}
        <button type="button" onClick={() => setOutcome(null)}>
          {outcome.kind === 'added' ? 'Add another station' : 'Back to the list'}
        </button>
      </>
    );
  } else if (device !== undefined && matched !== undefined) {
    body = card(matched);
  } else if (device !== undefined) {
    const ended = win !== null && !win.open && !opening;
    body = ended ? (
      <>
        <p role="status">The time to join ended.</p>
        <button type="button" onClick={() => void openWindow()}>
          Start again
        </button>
      </>
    ) : (
      <>
        <h3>Run this command on {device.host_name}</h3>
        <p>Open a terminal on {device.host_name} and run:</p>
        {device.command === null ? (
          <p className="fd-note" role="status">
            Preparing the command…
          </p>
        ) : (
          <CommandBlock command={device.command} />
        )}
        <WaitLine>{`Waiting for ${device.host_name} to ask to join…`}</WaitLine>
        <details className="fd-add-fold">
          <summary>What does this command do?</summary>
          <p className="fd-note">
            It asks this deck to add the computer as a station. It prints a code. In the next step
            you check that this deck shows the same code.
          </p>
        </details>
        <button type="button" className="fd-add-text-btn" onClick={() => setChosen(null)}>
          Choose another computer
        </button>
      </>
    );
  } else {
    body = (
      <DeviceList
        groups={groupDevices(list)}
        busy={opening}
        onAdd={(d) => void add(d.dns_name)}
        onReview={(d) => {
          setOutcome(null);
          setChosen(d.dns_name);
        }}
      />
    );
  }

  return (
    <section className="fd-add-flow" aria-label="A computer on my tailnet">
      <div className="fd-add-top">
        {available ? (
          <Steps labels={STEPS} current={step} done={outcome?.kind === 'added'} />
        ) : null}
        {win === null ? null : <WindowChip win={win} onStop={() => void closeWindow()} />}
      </div>
      {windowError === null ? null : <p role="alert">{windowError}</p>}
      {available && others.length > 0 ? (
        <section className="fd-add-others" aria-label="Other computers ask to join">
          <h3>Other computers ask to join</h3>
          {others.map(card)}
        </section>
      ) : null}
      {body}
      {error === null ? null : <p role="alert">{error}</p>}
      {node}
    </section>
  );
}
