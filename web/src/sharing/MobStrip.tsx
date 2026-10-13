// R11.7 (DL-166): the strip above the composer of a session in mob mode: who drives, "Request to
// drive" for a member, "Hand over" for each request and "Release" for the driver.
import { actorName, type DriveView } from './people';

type Props = {
  readonly view: DriveView;
  readonly busy?: boolean;
  /** The text of the last refusal or success of a drive call. */
  readonly note?: { readonly ok: boolean; readonly text: string } | null;
  readonly onRequest?: () => void;
  readonly onHandOver?: (to: string) => void;
  readonly onRelease?: () => void;
};

export function MobStrip({
  view,
  busy = false,
  note = null,
  onRequest,
  onHandOver,
  onRelease,
}: Props) {
  return (
    <section className="fd-mob" aria-label="Mob steering" data-driving={view.driving}>
      <div className="fd-mob-line">
        <span className="fd-mob-text">{view.text}</span>
        {view.canRequest ? (
          <button type="button" className="fd-sm fd-primary" disabled={busy} onClick={onRequest}>
            Request to drive
          </button>
        ) : view.requested ? (
          <span className="fd-badge" data-tone="muted">
            Drive requested
          </span>
        ) : null}
        {view.canRelease ? (
          <button type="button" className="fd-sm" disabled={busy} onClick={onRelease}>
            Release
          </button>
        ) : null}
      </div>
      {view.canHandOver && view.requests.length > 0 ? (
        <ul className="fd-mob-requests" aria-label="Requests to drive">
          {view.requests.map((actor) => (
            <li key={actor}>
              <span>{actorName(actor)} asks to drive</span>
              <button
                type="button"
                className="fd-sm"
                disabled={busy}
                aria-label={`Hand over to ${actorName(actor)}`}
                onClick={() => onHandOver?.(actor)}
              >
                Hand over
              </button>
            </li>
          ))}
        </ul>
      ) : null}
      {note === null ? null : (
        <p className="fd-note" role={note.ok ? 'status' : 'alert'}>
          {note.text}
        </p>
      )}
    </section>
  );
}
