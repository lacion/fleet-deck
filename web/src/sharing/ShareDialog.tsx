// R11.4 (DL-166): "Share…" (owner only): the role, the steering, the expiry and the uses of an
// invite, then the link with Copy and a QR code.
import { useEffect, useId, useRef, useState } from 'react';
import { trapTab, useFocusReturn } from '../app/focus';
import { useOverlay } from '../app/overlay';
import { sessionTitle } from '../app/sessions';
import { stationName } from '../board/fleet';
import { useSettings } from '../settings/context';
import type { ElevationAsk } from '../settings/elevation';
import { useFleetElevation } from '../spawn/useFleetElevation';
import type {
  DeckSession,
  NewShare,
  ShareCreated,
  ShareRole,
  SpaceView,
  StationView,
  Steering,
} from '../gen';
import { Icon, X } from '../ui/icons';
import { canShare, usePeople } from './context';
import { ROLE_TEXT, roleLabel } from './member';
import type { ShareCalls } from './people';
import { CopyLink, Qr } from './ShareLink';
import { closeShare, type ShareRequest, sharesChanged, useShareRequest } from './shareStore';

export { openShare } from './shareStore';

/** The expiry choices in days. */
export const EXPIRY_DAYS: readonly number[] = [1, 7, 30, 90];
/** The choices of how many people use one link. */
export const USES: readonly number[] = [1, 5, 25];
export const DEFAULT_DAYS = 7;
export const DEFAULT_USES = 1;

const ROLES: readonly ShareRole[] = ['viewer', 'collaborator', 'approver'];

const message = (e: unknown) => (e instanceof Error ? e.message : 'The request failed.');

/** The UTC date of a unix-ms time, `2026-10-13`. */
export const day = (ms: number): string => new Date(ms).toISOString().slice(0, 10);

export type ShareForm = {
  readonly role: ShareRole;
  readonly steering: Steering;
  readonly days: number;
  readonly uses: number;
  /** DL-189: members may read the earlier history of imported sessions. */
  readonly history: boolean;
};

export const defaultForm: ShareForm = {
  role: 'viewer',
  steering: 'open',
  days: DEFAULT_DAYS,
  uses: DEFAULT_USES,
  history: false,
};

/**
 * The body of `POST /api/shares`. Steering is for the roles that steer (collaborator, approver).
 * DL-189: `history` and `elevation` are in the body only when the history option is on.
 */
export function shareBody(request: ShareRequest, form: ShareForm, elevation?: string): NewShare {
  return {
    kind: request.kind,
    target: request.target,
    role: form.role,
    ...(form.role !== 'viewer' ? { steering: form.steering } : {}),
    expires_in_days: form.days,
    uses: form.uses,
    ...(form.history ? { history: true, ...(elevation === undefined ? {} : { elevation }) } : {}),
  };
}

/**
 * DL-189: makes the share. The history option needs the owner's elevation first; `null` when the
 * owner cancels it (no request is sent then).
 */
export async function createShare(
  calls: Pick<ShareCalls, 'create'>,
  ask: ElevationAsk,
  request: ShareRequest,
  form: ShareForm,
): Promise<ShareCreated | null> {
  if (!form.history) return calls.create(shareBody(request, form));
  const token = await ask(['Share the earlier history']);
  if (token === null) return null;
  return calls.create(shareBody(request, form, token));
}

/** One thing that "New share" can share: the board, a space or a session. */
export type ShareTarget = {
  readonly value: string;
  readonly label: string;
  readonly group: 'board' | 'space' | 'session';
  readonly request: ShareRequest;
};

/** The targets of "New share": the whole board, the spaces (when any), then the sessions by title. */
export function shareTargets(
  sessions: readonly DeckSession[],
  stations: readonly StationView[],
  spaces: readonly SpaceView[],
): readonly ShareTarget[] {
  return [
    {
      value: 'board',
      label: 'The whole board',
      group: 'board',
      request: { kind: 'board', target: '', label: '' },
    },
    ...spaces.map(
      (space): ShareTarget => ({
        value: `space:${space.space_id}`,
        label: `Space ${space.name}`,
        group: 'space',
        request: { kind: 'space', target: space.space_id, label: space.name },
      }),
    ),
    ...sessions.map((card): ShareTarget => {
      const title = sessionTitle(card);
      return {
        value: `session:${card.station_id}/${card.summary.session_id}`,
        label: `${title} (${stationName(card.station_id, stations)})`,
        group: 'session',
        request: {
          kind: 'session',
          target: `${card.station_id}/${card.summary.session_id}`,
          label: title,
        },
      };
    }),
  ];
}

const TITLE: Record<ShareRequest['kind'], string> = {
  session: 'Share session',
  board: 'Share board',
  space: 'Share space',
};

/** The new link: the URL with Copy, the QR code and the line about who can join. */
export function ShareResult({
  link,
  joinAs,
}: {
  readonly link: ShareCreated;
  /** The role that the link gives. */
  readonly joinAs: ShareRole;
}) {
  return (
    <div className="fd-share-result" role="status">
      <CopyLink url={link.url} />
      <Qr url={link.url} />
      <p className="fd-note">
        Anyone with this link can join as {roleLabel(joinAs)} until {day(link.expires_at)}. Revoke
        it in Shares.
      </p>
    </div>
  );
}

type FormProps = {
  readonly request: ShareRequest;
  readonly form: ShareForm;
  readonly busy: boolean;
  readonly onChange: (form: ShareForm) => void;
  readonly onSubmit: () => void;
};

/** The fields: role, steering (collaborator), expiry and uses. */
export function ShareFields({ request, form, busy, onChange, onSubmit }: FormProps) {
  const name = useId();
  return (
    <form
      className="fd-share-form"
      aria-label={`${TITLE[request.kind]} form`}
      onSubmit={(e) => {
        e.preventDefault();
        onSubmit();
      }}
    >
      <fieldset className="fd-share-roles">
        <legend>Role</legend>
        {ROLES.map((role) => (
          <label key={role} className="fd-share-role">
            <input
              type="radio"
              name={`${name}-role`}
              value={role}
              checked={form.role === role}
              onChange={() => onChange({ ...form, role })}
            />
            <span>
              <strong>{role.charAt(0).toUpperCase() + role.slice(1)}</strong>{' '}
              <span className="fd-note">{ROLE_TEXT[role]}</span>
            </span>
          </label>
        ))}
      </fieldset>
      {form.role !== 'viewer' ? (
        <fieldset className="fd-share-roles">
          <legend>Steering</legend>
          <label className="fd-share-role">
            <input
              type="radio"
              name={`${name}-steering`}
              value="open"
              checked={form.steering === 'open'}
              onChange={() => onChange({ ...form, steering: 'open' })}
            />
            <span>Open (anyone steers)</span>
          </label>
          <label className="fd-share-role">
            <input
              type="radio"
              name={`${name}-steering`}
              value="mob"
              checked={form.steering === 'mob'}
              onChange={() => onChange({ ...form, steering: 'mob' })}
            />
            <span>Mob (one driver at a time)</span>
          </label>
        </fieldset>
      ) : null}
      <label>
        Expires after
        <select
          value={form.days}
          onChange={(e) => onChange({ ...form, days: Number(e.target.value) })}
        >
          {EXPIRY_DAYS.map((d) => (
            <option key={d} value={d}>
              {d === 1 ? '1 day' : `${d} days`}
            </option>
          ))}
        </select>
      </label>
      <label>
        Who can use it
        <select
          value={form.uses}
          onChange={(e) => onChange({ ...form, uses: Number(e.target.value) })}
        >
          {USES.map((n) => (
            <option key={n} value={n}>
              {n === 1 ? '1 person' : `${n} people`}
            </option>
          ))}
        </select>
      </label>
      <label className="fd-share-role">
        <input
          type="checkbox"
          checked={form.history}
          onChange={(e) => onChange({ ...form, history: e.target.checked })}
        />
        <span>People with the link can read the history from before the deck ran a session</span>
      </label>
      <p className="fd-note">
        They see the full earlier conversation, including anything pasted into it. Tool commands and
        file names stay hidden.
      </p>
      <button type="submit" className="fd-primary" disabled={busy}>
        {busy ? 'Making the link…' : 'Make a link'}
      </button>
    </form>
  );
}

type Props = {
  readonly request: ShareRequest;
  readonly calls: Pick<ShareCalls, 'create'>;
  readonly onClose: () => void;
  /** A link, so that a static render shows the result (tests). */
  readonly initialLink?: { readonly link: ShareCreated; readonly role: ShareRole };
  /** An error text, so that a static render shows it (tests). */
  readonly initialError?: string;
  /** The form values at the first render (tests). */
  readonly initialForm?: ShareForm;
  /** "New share": the dialog starts with a select of what to share; `request` is not used then. */
  readonly targets?: readonly ShareTarget[];
  /** The value of the target at the first render (tests). */
  readonly initialTarget?: string;
};

/** "Share…": the dialog of one share request (a session, a space or the whole board). */
export function ShareDialog({
  request,
  calls,
  onClose,
  initialLink,
  initialError,
  initialForm,
  targets,
  initialTarget,
}: Props) {
  const root = useRef<HTMLDivElement>(null);
  const close = useRef<HTMLButtonElement>(null);
  useFocusReturn(root, close);
  useOverlay();
  const [pick, setPick] = useState(initialTarget ?? targets?.[0]?.value ?? '');
  const active = targets?.find((t) => t.value === pick)?.request ?? request;
  const [form, setForm] = useState<ShareForm>(initialForm ?? defaultForm);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(initialError ?? null);
  const [made, setMade] = useState(initialLink ?? null);
  const alive = useRef(true);
  const { ask, node: elevationNode } = useFleetElevation(useSettings());
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const create = async () => {
    setBusy(true);
    setError(null);
    setMade(null);
    try {
      const link = await createShare(calls, ask, active, form);
      if (!alive.current || link === null) return;
      setMade({ link, role: form.role });
      sharesChanged();
    } catch (e) {
      if (alive.current) setError(message(e));
    } finally {
      if (alive.current) setBusy(false);
    }
  };

  const title = targets === undefined ? TITLE[request.kind] : 'New share';
  return (
    // The overlay click is a mouse shortcut; the Close button and Escape serve the keyboard.
    <div
      className="fd-overlay"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        ref={root}
        className="fd-dialog fd-share"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onKeyDown={(e) => {
          if (e.key === 'Escape') {
            e.stopPropagation();
            onClose();
          } else trapTab(e, root.current);
        }}
      >
        <div className="fd-dialog-head">
          <h2>{title}</h2>
          {targets === undefined && request.label !== '' ? (
            <p className="fd-note">{request.label}</p>
          ) : null}
          <button
            ref={close}
            type="button"
            className="fd-iconbtn"
            aria-label="Close"
            title="Close"
            onClick={onClose}
          >
            <Icon of={X} />
          </button>
        </div>
        {targets === undefined ? null : (
          <label className="fd-share-target">
            What to share
            <select
              value={pick}
              onChange={(e) => {
                setPick(e.target.value);
                setMade(null);
                setError(null);
              }}
            >
              {targets
                .filter((t) => t.group === 'board')
                .map((t) => (
                  <option key={t.value} value={t.value}>
                    {t.label}
                  </option>
                ))}
              {targets.some((t) => t.group === 'space') ? (
                <optgroup label="Spaces">
                  {targets
                    .filter((t) => t.group === 'space')
                    .map((t) => (
                      <option key={t.value} value={t.value}>
                        {t.label}
                      </option>
                    ))}
                </optgroup>
              ) : null}
              {targets.some((t) => t.group === 'session') ? (
                <optgroup label="Sessions">
                  {targets
                    .filter((t) => t.group === 'session')
                    .map((t) => (
                      <option key={t.value} value={t.value}>
                        {t.label}
                      </option>
                    ))}
                </optgroup>
              ) : null}
            </select>
          </label>
        )}
        <ShareFields
          request={active}
          form={form}
          busy={busy}
          onChange={setForm}
          onSubmit={() => void create()}
        />
        {error === null ? null : (
          <p className="fd-note fd-share-error" role="alert">
            {error}
          </p>
        )}
        {made === null ? null : <ShareResult link={made.link} joinAs={made.role} />}
      </div>
      {/* DL-189: beside the dialog, so that its keys do not reach the keys of the share dialog. */}
      {elevationNode}
    </div>
  );
}

/** "New share" of the Shares page: the dialog with a select of the board, the spaces and the sessions. */
export function NewShareDialog({
  sessions,
  stations,
  spaces,
  calls,
  onClose,
}: {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly spaces: readonly SpaceView[];
  readonly calls: Pick<ShareCalls, 'create'>;
  readonly onClose: () => void;
}) {
  const targets = shareTargets(sessions, stations, spaces);
  const first = targets[0];
  if (first === undefined) return null;
  return <ShareDialog request={first.request} targets={targets} calls={calls} onClose={onClose} />;
}

/** Renders the dialog when an entry point asked for it; mount it once. */
export function ShareHost() {
  const request = useShareRequest();
  const people = usePeople();
  if (request === null || !canShare(people) || people.shares === undefined) return null;
  return (
    <ShareDialog
      key={`${request.kind}:${request.target}`}
      request={request}
      calls={people.shares}
      onClose={closeShare}
    />
  );
}
