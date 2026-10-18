// R13 P6 follow-up (design `omarchy-p6.md` §6.4): the preview windows that agents opened on a
// station. The owner sees each one and closes it. A row shows the label of the agent caller and
// the monitor when it is known; the station gives no address, URL or profile.
import type { DesktopInfo, PreviewWindow } from '../gen';
import type { DesktopActions } from '../displays/actions';

/** The heading of the section. */
export const PREVIEWS_TITLE = 'Agent previews';

/** The text of a row: the label of the caller, and ` · <monitor>` when the monitor is known. */
export const previewText = (p: PreviewWindow): string =>
  p.monitor === null ? p.caller_label : `${p.caller_label} · ${p.monitor}`;

/**
 * Closes one preview: the owner act `preview_close` with the handle (one POST, never repeated),
 * then a refresh of the desktop info. `onInfo` receives the info of the answer at once and the
 * refreshed info after it. An error of the act or of the refresh rejects with that error.
 */
export async function closePreview(
  desktop: DesktopActions,
  station: string,
  windowId: string,
  onInfo: (info: DesktopInfo) => void,
): Promise<void> {
  const result = await desktop.act(station, { action: 'preview_close', window_id: windowId });
  onInfo(result.info);
  onInfo(await desktop.info(station));
}

export function AgentPreviews({
  previews,
  busy,
  onClose,
}: {
  readonly previews: readonly PreviewWindow[] | undefined;
  readonly busy: boolean;
  readonly onClose: (windowId: string) => void;
}) {
  if (previews === undefined || previews.length === 0) return null;
  return (
    <section className="fd-desk-previews" aria-label={PREVIEWS_TITLE}>
      <p>
        <strong>{PREVIEWS_TITLE}</strong>
      </p>
      <ul className="fd-desk-walls">
        {previews.map((p) => (
          <li key={p.window_id} data-preview={p.window_id}>
            <span>{previewText(p)}</span>
            <button
              type="button"
              className="fd-ghost fd-sm"
              disabled={busy}
              onClick={() => onClose(p.window_id)}
            >
              Close
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}
