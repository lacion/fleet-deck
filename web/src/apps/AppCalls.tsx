// B15 (design `docs/v2/design/mcp-apps.md` §4.8.2, §4.8.3): the open-calls strip of the chat. It lists
// the open app calls of the session and of its folder, also calls of an app that is gone, and
// lets the owner release an `uncertain` call with the call's own session.
import { useEffect, useState, useSyncExternalStore } from 'react';
import type { AppActions } from './actions';
import type { AppOpenCall } from '../gen';
import { Dialog } from '../integrations/Dialog';
import { ApiError } from '../runtime/api';
import { Icon, TriangleAlert } from '../ui/icons';
import { type AppsEnv, callsVersion, subscribeCalls } from './env';

export const POLL_MS = 30_000;
export const OTHER_SESSION = ' It belongs to another session in this folder.';
export const STILL_RUNNING = 'This app action is still running. Stop the session to end it.';

const WAITS = 'A revert of this folder waits until it ends.';

/** The words of one open call (§4.8.3). `release` is true for a line with the button. */
export function callLine(
  call: AppOpenCall,
  session: string,
): { readonly text: string; readonly release: boolean } {
  const of = `${call.tool} from ${call.server}`;
  const tail = call.session_id === session ? '' : OTHER_SESSION;
  if (call.state === 'running') {
    return { text: `An app action is running: ${of}. ${WAITS}${tail}`, release: false };
  }
  const lead =
    call.cause === 'provider_error'
      ? `The provider stopped waiting for an app action, but the tool may still run: ${of}.`
      : `An app action may still be running after a station restart: ${of}.`;
  return { text: `${lead} ${WAITS}${tail}`, release: true };
}

/** The result of the release flow, for the dialog. */
export type ReleaseResult =
  | { readonly kind: 'done' }
  | { readonly kind: 'running'; readonly text: string }
  | { readonly kind: 'failed'; readonly text: string };

/**
 * Posts the release to the route of the call's own session. `released: false` is `done` too: the
 * line goes away without a message. `call_running` has its own notice.
 */
export async function releaseCall(
  actions: AppActions,
  station: string,
  call: AppOpenCall,
): Promise<ReleaseResult> {
  try {
    await actions.release(station, call.session_id, call.call_id);
    return { kind: 'done' };
  } catch (e) {
    if (e instanceof ApiError && e.code === 'call_running') {
      return { kind: 'running', text: STILL_RUNNING };
    }
    return { kind: 'failed', text: e instanceof Error ? e.message : 'The release failed.' };
  }
}

/** The lines of the strip; a line with `release` has the button "It has stopped". */
export function CallLines(props: {
  readonly calls: readonly AppOpenCall[];
  readonly session: string;
  readonly onRelease: (call: AppOpenCall) => void;
}) {
  if (props.calls.length === 0) return null;
  return (
    <div className="fd-app-calls">
      {props.calls.map((call) => {
        const line = callLine(call, props.session);
        if (!line.release) {
          return (
            <p key={call.call_id} className="fd-note fd-app-call" data-state="running">
              {line.text}
            </p>
          );
        }
        return (
          <div
            key={call.call_id}
            className="fd-notice fd-notice-row"
            data-tone="warn"
            data-place="page"
          >
            <Icon of={TriangleAlert} size={14} />
            <span className="fd-notice-text">{line.text}</span>
            <button type="button" onClick={() => props.onRelease(call)}>
              It has stopped
            </button>
          </div>
        );
      })}
    </div>
  );
}

/** The release dialog (§4.8.3). */
export function ReleaseDialog(props: {
  readonly call: AppOpenCall;
  readonly notice: string | null;
  readonly busy: boolean;
  readonly onCancel: () => void;
  readonly onConfirm: () => void;
}) {
  return (
    <Dialog
      title="Mark this app action as stopped?"
      description={`Only do this when you know that ${props.call.tool} is not running. A revert can then change the files that it writes.`}
      onClose={props.onCancel}
    >
      <div className="fd-int-form">
        {props.notice === null ? null : (
          <p className="fd-note fd-int-error" role="alert">
            {props.notice}
          </p>
        )}
        <div className="fd-pane-row">
          <button type="button" disabled={props.busy} onClick={props.onCancel}>
            Cancel
          </button>
          <button
            type="button"
            className="fd-decide"
            disabled={props.busy}
            onClick={props.onConfirm}
          >
            Mark as stopped
          </button>
        </div>
      </div>
    </Dialog>
  );
}

/**
 * The strip. It asks the list route on mount, when `revertKey` changes, when `refreshCalls()`
 * runs, and every 30 s while the list is not empty. It renders nothing for a member, for an
 * empty list and after `apps_unsupported`.
 */
export function AppCalls({ env }: { readonly env: AppsEnv }) {
  const { station, session, actions, owner, revertKey } = env;
  const bumped = useSyncExternalStore(
    (fn) => subscribeCalls(station, session, fn),
    () => callsVersion(station, session),
    () => 0,
  );
  const [calls, setCalls] = useState<readonly AppOpenCall[]>([]);
  const [off, setOff] = useState(false);
  const [asking, setAsking] = useState<AppOpenCall | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [again, setAgain] = useState(0);
  const polling = calls.length > 0;

  useEffect(() => {
    if (!owner || off) return undefined;
    let live = true;
    void actions.calls(station, session).then(
      (reply) => {
        if (live) setCalls(reply.calls);
      },
      (e) => {
        if (live && e instanceof ApiError && e.code === 'apps_unsupported') setOff(true);
      },
    );
    return () => {
      live = false;
    };
  }, [owner, off, actions, station, session, revertKey, bumped, again]);

  useEffect(() => {
    if (!owner || off || !polling) return undefined;
    const timer = setInterval(() => setAgain((n) => n + 1), POLL_MS);
    return () => clearInterval(timer);
  }, [owner, off, polling]);

  if (!owner || off) return null;
  const release = async () => {
    if (asking === null) return;
    setBusy(true);
    const result = await releaseCall(actions, station, asking);
    setBusy(false);
    if (result.kind === 'done') {
      setAsking(null);
      setNotice(null);
      setAgain((n) => n + 1);
    } else {
      setNotice(result.text);
    }
  };
  return (
    <>
      <CallLines
        calls={calls}
        session={session}
        onRelease={(call) => {
          setNotice(null);
          setAsking(call);
        }}
      />
      {asking === null ? null : (
        <ReleaseDialog
          call={asking}
          notice={notice}
          busy={busy}
          onCancel={() => setAsking(null)}
          onConfirm={() => void release()}
        />
      )}
    </>
  );
}
