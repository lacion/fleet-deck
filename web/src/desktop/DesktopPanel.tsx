// R13 P3c (design §6.2, §6.3, §6D): the Desktop panel of a station card. It shows the desktop that
// the station reports and sends the closed list of `desktop.act` actions of the owner's relay.
// The deck decides every limit and answers with fixed texts, which the panel shows as they are.
// A press sends one request and never repeats it: an action whose result is not known can have
// run on the station.
import { useState } from 'react';
import { formatHash } from '../app/routes';
import type { DesktopActBody, DesktopInfo, StationView } from '../gen';
import { useSettings } from '../settings/context';
import { describe } from '../settings/http';
import { AgentPreviews, closePreview } from './AgentPreviews';
import { useCapture } from './CapturePanel';
import { LinkForm, NotifyForm, type Run, WallOpenForm, WorkspaceForm } from './DesktopForms';
import { useRecording } from './RecordPanel';

type Confirm = { readonly text: string; readonly body: DesktopActBody; readonly done: string };

const screenText = (on: boolean | null): string =>
  on === null ? 'Screen unknown' : on ? 'Screen on' : 'Screen off';

const yesNo = (v: boolean | null): string => (v === null ? 'unknown' : v ? 'on' : 'off');

export function DesktopPanel({ station }: { readonly station: StationView }) {
  const env = useSettings();
  const desktop = station.info?.desktop;
  const [info, setInfo] = useState<DesktopInfo | undefined>(desktop);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<Confirm | null>(null);
  const [address, setAddress] = useState<string | null>(null);
  const shown = info ?? desktop;
  const capture = useCapture(station, shown, busy);
  const record = useRecording(station, shown, busy, capture.deck);
  if (env === undefined) return null;
  if (shown === undefined) {
    return station.info === null ? null : (
      <p className="fd-note">This station reports no desktop.</p>
    );
  }
  const send = (body: DesktopActBody, done: string) => {
    setBusy(true);
    setError(null);
    setNote(null);
    setConfirm(null);
    env.desktop.act(station.name, body).then(
      (result) => {
        setBusy(false);
        setInfo(result.info);
        setNote(done);
        if (body.action === 'browser_open') setAddress(result.address);
        if (body.action === 'window_close') setAddress(null);
      },
      (e: unknown) => {
        setBusy(false);
        setError(describe(e));
      },
    );
  };
  const run: Run = send;
  const closeOne = (windowId: string) => {
    setBusy(true);
    setError(null);
    setNote(null);
    setConfirm(null);
    closePreview(env.desktop, station.name, windowId, setInfo).then(
      () => {
        setBusy(false);
        setNote('The preview is closed.');
      },
      (e: unknown) => {
        setBusy(false);
        setError(describe(e));
      },
    );
  };
  const refresh = () => {
    setBusy(true);
    setError(null);
    setNote(null);
    env.desktop.info(station.name).then(
      (next) => {
        setBusy(false);
        setInfo(next);
      },
      (e: unknown) => {
        setBusy(false);
        setError(describe(e));
      },
    );
  };
  return (
    <details className="fd-desk" data-station={station.station_id}>
      <summary data-variant="toggle">Desktop</summary>
      <dl className="fd-desk-facts">
        <div>
          <dt>Desktop</dt>
          <dd>
            {shown.kind === 'omarchy' ? 'Omarchy' : 'Hyprland'}
            {shown.omarchy_version === null ? '' : ` ${shown.omarchy_version}`}
          </dd>
        </div>
        <div>
          <dt>Locked</dt>
          <dd>{yesNo(shown.locked)}</dd>
        </div>
        <div>
          <dt>Do not disturb</dt>
          <dd>{yesNo(shown.dnd)}</dd>
        </div>
        <div>
          <dt>Idle inhibit</dt>
          <dd>{yesNo(shown.idle_inhibited)}</dd>
        </div>
        <div>
          <dt>Desktop actions</dt>
          <dd>{shown.allowed.desktop ? 'allowed on the station' : 'off on the station'}</dd>
        </div>
      </dl>
      <ul className="fd-desk-monitors" aria-label="Monitors">
        {shown.monitors.map((m) => (
          <li key={m.name} data-monitor={m.name}>
            <span className="fd-desk-monitor">
              <strong>{m.name}</strong> · {m.width}×{m.height} · scale {m.scale_pct / 100} ·{' '}
              {m.refresh_hz} Hz{m.focused ? ' · focused' : ''} · {screenText(m.dpms_on)}
            </span>
            <span className="fd-desk-buttons">
              {capture.button(m.name, record.active)}
              <button
                type="button"
                className="fd-sm"
                disabled={busy}
                onClick={() =>
                  send({ action: 'screen_power', monitor: m.name, on: true }, `${m.name} is on.`)
                }
              >
                Screen on
              </button>
              <button
                type="button"
                className="fd-sm"
                disabled={busy}
                onClick={() =>
                  setConfirm({
                    text: `Turn off ${m.name} on ${station.name}?`,
                    body: { action: 'screen_power', monitor: m.name, on: false },
                    done: `${m.name} is off.`,
                  })
                }
              >
                Screen off
              </button>
            </span>
            {record.controls(m.name, capture.active)}
          </li>
        ))}
      </ul>
      {capture.reason === null ? null : <p className="fd-note">{capture.reason}</p>}
      {record.reason === null ? null : <p className="fd-note">{record.reason}</p>}
      {capture.block}
      {record.block}
      <div className="fd-desk-buttons">
        <button
          type="button"
          className="fd-sm"
          disabled={busy}
          onClick={() =>
            setConfirm({
              text: `Lock the screen of ${station.name}?`,
              body: { action: 'screen_lock' },
              done: 'The screen is locked.',
            })
          }
        >
          Lock the screen
        </button>
        <label className="fd-disp-check">
          <input
            type="checkbox"
            role="switch"
            className="fd-switch"
            aria-checked={shown.dnd === true}
            checked={shown.dnd === true}
            disabled={busy}
            onChange={(e) =>
              send({ action: 'screen_dnd', on: e.target.checked }, 'Do not disturb changed.')
            }
          />{' '}
          Do not disturb
        </label>
        <label className="fd-disp-check">
          <input
            type="checkbox"
            role="switch"
            className="fd-switch"
            aria-checked={shown.idle_inhibited === true}
            checked={shown.idle_inhibited === true}
            disabled={busy}
            onChange={(e) =>
              send({ action: 'screen_idle_inhibit', on: e.target.checked }, 'Idle inhibit changed.')
            }
          />{' '}
          Keep the screen awake
        </label>
        <button type="button" className="fd-sm" disabled={busy} onClick={refresh}>
          Refresh
        </button>
      </div>
      {confirm === null ? null : (
        <div className="fd-disp-confirm" role="alertdialog" aria-label="Confirm">
          <p>{confirm.text}</p>
          <button
            type="button"
            className="fd-primary"
            disabled={busy}
            onClick={() => send(confirm.body, confirm.done)}
          >
            Confirm
          </button>
          <button type="button" className="fd-ghost" onClick={() => setConfirm(null)}>
            Cancel
          </button>
        </div>
      )}
      <WorkspaceForm info={shown} run={run} busy={busy} />
      <NotifyForm run={run} busy={busy} />
      <LinkForm info={shown} run={run} busy={busy} />
      {address === null ? null : (
        <p className="fd-desk-window">
          Opened window {address}{' '}
          <button
            type="button"
            className="fd-sm"
            disabled={busy}
            onClick={() => send({ action: 'window_close', address }, 'Window closed.')}
          >
            Close this window
          </button>
        </p>
      )}
      <WallOpenForm info={shown} run={run} busy={busy} displays={env.displays} />
      {shown.walls.length === 0 ? null : (
        <ul className="fd-desk-walls" aria-label="Open walls">
          {shown.walls.map((w) => (
            <li key={w.address} data-display={w.display_id}>
              <span className="fd-desk-monitor">
                Wall {w.display_id}
                {w.monitor === null ? '' : ` on ${w.monitor}`}
              </span>
              <span className="fd-desk-buttons">
                <button
                  type="button"
                  className="fd-sm"
                  onClick={() => {
                    window.location.hash = formatHash({ name: 'remote', id: w.display_id });
                  }}
                >
                  Remote
                </button>
                <button
                  type="button"
                  className="fd-sm"
                  disabled={busy}
                  onClick={() =>
                    send({ action: 'wall_reload', display_id: w.display_id }, 'Reloading the wall.')
                  }
                >
                  Reload
                </button>
                <button
                  type="button"
                  className="fd-sm"
                  disabled={busy}
                  onClick={() =>
                    setConfirm({
                      text: `Close the wall on ${station.name}?`,
                      body: { action: 'wall_close', display_id: w.display_id },
                      done: 'The wall is closed.',
                    })
                  }
                >
                  Close
                </button>
              </span>
            </li>
          ))}
        </ul>
      )}
      <AgentPreviews previews={shown.previews} busy={busy} onClose={closeOne} />
      {error === null ? null : (
        <p className="fd-fielderror" role="alert">
          {error}
        </p>
      )}
      {note === null ? null : <p role="status">{note}</p>}
    </details>
  );
}
