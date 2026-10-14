// DL-178: the "On your tailnet" tab of the Add station dialog: the request window, the devices
// that the deck's Tailscale client sees with the station command, and the waiting pair requests.
// Nothing pairs by itself: the owner compares the fingerprint and approves (with an elevation).
import { useCallback, useEffect, useRef, useState } from 'react';
import type { SettingsEnv } from '../settings/context';
import { useElevation } from '../settings/elevation';
import { DeckError, describe } from '../settings/http';
import type { Device, DevicesView, PairRequest, RequestsView, WindowView } from './api';
import { approveError, clockText, groupFingerprint, windowText } from './model';

const REFRESH_MS = 3_000;
const COPIED_MS = 1_500;
const NO_LISTENER_TEXT =
  'The deck has no tailnet link address yet. Use tailnet in Settings → Deck, then restart the deck.';

/** One waiting request. The values on the card are the ones that the owner approves. */
export function RequestCard({
  request,
  onApprove,
  onDeny,
  error,
  busy = false,
}: {
  readonly request: PairRequest;
  readonly onApprove: () => void;
  readonly onDeny: () => void;
  readonly error: string | null;
  readonly busy?: boolean;
}) {
  const t = request.tailscale;
  const identity =
    t !== null && (t.host_name !== undefined || t.login !== undefined) && t.error === undefined
      ? `Tailscale: ${[
          t.host_name,
          t.login,
          t.tags.length === 0 ? undefined : `tags ${t.tags.join(', ')}`,
        ]
          .filter((part) => part !== undefined)
          .join(' · ')}`
      : 'Tailscale identity unavailable';
  return (
    <article className="fd-pair-request" aria-label={`Pair request ${request.requested_name}`}>
      <p>Requested name: {request.requested_name}</p>
      <p>
        Link key fingerprint:{' '}
        <code className="fd-fingerprint">{groupFingerprint(request.station_fingerprint)}</code>
      </p>
      <p>Scopes: {request.offered_scopes.join(', ')}</p>
      <p>From {request.source_ip}</p>
      <p>{identity}</p>
      <p>Expires {clockText(request.expires_at)}</p>
      <p className="fd-note">
        Compare this fingerprint with the one that the station printed. Approve only when they are
        equal.
      </p>
      <div className="fd-pair-actions">
        <button type="button" className="fd-primary" disabled={busy} onClick={onApprove}>
          Approve
        </button>
        <button type="button" disabled={busy} onClick={onDeny}>
          Deny
        </button>
      </div>
      {error === null ? null : <p role="alert">{error}</p>}
    </article>
  );
}

function DeviceRow({ device }: { readonly device: Device }) {
  const [copy, setCopy] = useState<'idle' | 'copied' | 'failed'>('idle');
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(
    () => () => {
      if (timer.current !== null) clearTimeout(timer.current);
    },
    [],
  );
  const command = device.command;
  const copyCommand = async () => {
    if (command === null) return;
    try {
      await navigator.clipboard.writeText(command);
      setCopy('copied');
      if (timer.current !== null) clearTimeout(timer.current);
      timer.current = setTimeout(() => setCopy('idle'), COPIED_MS);
    } catch {
      setCopy('failed');
    }
  };
  let state: React.ReactNode;
  if (device.state === 'paired') state = 'Paired';
  else if (device.state === 'request waiting') state = 'Request waiting';
  else if (command === null) state = 'Open the window to get the station command.';
  else {
    state = (
      <>
        <code>{command}</code>{' '}
        <button type="button" onClick={() => void copyCommand()}>
          {copy === 'copied' ? 'Copied' : 'Copy'}
        </button>
        {copy === 'failed' ? <span role="alert"> Copy failed: select the text</span> : null}
      </>
    );
  }
  return (
    <li data-device={device.dns_name}>
      <strong>{device.host_name}</strong> {device.dns_name} ({device.ips.join(', ')})
      {device.os === undefined ? null : ` ${device.os}`} {device.online ? 'online' : 'offline'}
      {device.tags.length === 0 ? null : ` ${device.tags.join(', ')}`}
      <div>{state}</div>
    </li>
  );
}

export function TailnetPairing({ env }: { readonly env: SettingsEnv }) {
  const { tailnet } = env;
  const { ask, node } = useElevation(env.settings);
  const [win, setWin] = useState<WindowView | null>(null);
  const [requests, setRequests] = useState<RequestsView['requests']>([]);
  const [devices, setDevices] = useState<DevicesView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [windowError, setWindowError] = useState<string | null>(null);
  const [requestErrors, setRequestErrors] = useState<Readonly<Record<string, string>>>({});
  const [paired, setPaired] = useState<readonly string[]>([]);
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

  const openWindow = async () => {
    setWindowError(null);
    try {
      const elevation = await ask(['Pairing window']);
      if (elevation === null) return;
      await tailnet.setWindow(true, elevation);
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
      if (alive.current) setPaired((prev) => [...prev, r.requested_name]);
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
    } catch (e) {
      fail(r.request_id, describe(e));
    } finally {
      if (alive.current) setActing(null);
    }
    await refresh();
  };

  return (
    <section aria-label="On your tailnet">
      {win === null ? null : win.open ? (
        <>
          <p role="status">{windowText(win, Date.now())}</p>
          <button type="button" onClick={() => void closeWindow()}>
            Close
          </button>
        </>
      ) : (
        <button type="button" onClick={() => void openWindow()}>
          Accept pairing requests for 10 minutes
        </button>
      )}
      {windowError === null ? null : <p role="alert">{windowError}</p>}

      <h3>Devices</h3>
      {devices === null ? (
        <p className="fd-note">Checking Tailscale…</p>
      ) : !devices.available ? (
        <p className="fd-note">
          Tailscale is not available on the deck machine: {devices.reason ?? 'no reason given'}.
        </p>
      ) : (
        <ul className="fd-tailnet-devices">
          {devices.devices.map((d) => (
            <DeviceRow key={d.dns_name} device={d} />
          ))}
        </ul>
      )}

      <h3>Waiting requests</h3>
      {paired.map((name) => (
        <p key={name} role="status">
          Paired {name}. Waiting for the station to connect.
        </p>
      ))}
      {requests.length === 0 ? (
        <p className="fd-note">No waiting requests.</p>
      ) : (
        requests.map((r) => (
          <RequestCard
            key={r.request_id}
            request={r}
            busy={acting === r.request_id}
            error={requestErrors[r.request_id] ?? null}
            onApprove={() => void approve(r)}
            onDeny={() => void deny(r)}
          />
        ))
      )}
      {error === null ? null : <p role="alert">{error}</p>}
      {node}
    </section>
  );
}
