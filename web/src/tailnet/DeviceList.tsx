// Step 1 of the tailnet flow: the devices of the tailnet in groups. Only a device in "Can be
// added" gets an Add button; a device with a waiting request gets a Review button.
import type { ReactNode } from 'react';
import type { Device } from './api';
import { type DeviceGroups, osLabel } from './model';

const IPHONE_TEXT = 'Phones and tablets cannot run Fleet Deck.';

function Row({
  device,
  label,
  action,
}: {
  readonly device: Device;
  readonly label: string;
  readonly action?: ReactNode;
}) {
  return (
    <li className="fd-add-device" data-device={device.dns_name} title={device.ips.join(', ')}>
      <span className="fd-add-dot" data-online={device.online} aria-hidden="true" />
      <span className="fd-add-device-main">
        <span className="fd-add-device-line">
          <strong>{device.host_name}</strong>
          <span className="fd-add-device-os">{label}</span>
        </span>
        <span className="fd-add-device-dns">{device.dns_name}</span>
      </span>
      {action}
    </li>
  );
}

function Fold({
  title,
  rows,
  note,
  label,
}: {
  readonly title: string;
  readonly rows: readonly Device[];
  readonly note?: string;
  /** A fixed row label; else the operating system. */
  readonly label?: string;
}) {
  if (rows.length === 0) return null;
  return (
    <details className="fd-add-fold">
      <summary>
        {title} ({rows.length})
      </summary>
      {note === undefined ? null : <p className="fd-note">{note}</p>}
      <ul className="fd-add-list">
        {rows.map((d) => (
          <Row key={d.dns_name} device={d} label={label ?? osLabel(d.os)} />
        ))}
      </ul>
    </details>
  );
}

export function DeviceList({
  groups,
  busy,
  onAdd,
  onReview,
}: {
  readonly groups: DeviceGroups;
  readonly busy: boolean;
  readonly onAdd: (device: Device) => void;
  readonly onReview: (device: Device) => void;
}) {
  return (
    <>
      <p>Choose the computer that will run the station. Fleet Deck must be installed on it.</p>
      <section className="fd-add-group" aria-label="Can be added">
        <h3>Can be added</h3>
        {groups.addable.length === 0 ? (
          <p className="fd-note">No computer on your tailnet can be added now.</p>
        ) : (
          <ul className="fd-add-list">
            {groups.addable.map((d) => (
              <Row
                key={d.dns_name}
                device={d}
                label={osLabel(d.os)}
                action={
                  <button
                    type="button"
                    className="fd-primary fd-add-small-btn"
                    disabled={busy}
                    onClick={() => onAdd(d)}
                  >
                    Add
                  </button>
                }
              />
            ))}
          </ul>
        )}
      </section>
      {groups.waiting.length === 0 ? null : (
        <section className="fd-add-group" aria-label="Waiting for your answer">
          <h3>Waiting for your answer</h3>
          <ul className="fd-add-list">
            {groups.waiting.map((d) => (
              <Row
                key={d.dns_name}
                device={d}
                label={osLabel(d.os)}
                action={
                  <button type="button" className="fd-add-small-btn" onClick={() => onReview(d)}>
                    Review
                  </button>
                }
              />
            ))}
          </ul>
        </section>
      )}
      <Fold title="Already stations" rows={groups.paired} label="Station" />
      <Fold title="Offline" rows={groups.offline} />
      <Fold title="Cannot run a station" rows={groups.unsupported} note={IPHONE_TEXT} />
    </>
  );
}
