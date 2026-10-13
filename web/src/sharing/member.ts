// R11 (DL-166): who is on the board. A member is a guest of the owner's deck: it has a role and
// sees what its share covers. The owner and a token have no member.
import type { ShareRole, SharedScope } from '../gen';
import type { AuthView } from '../runtime/state';

export type Member = {
  readonly role: ShareRole;
  readonly name: string;
  readonly scope: SharedScope | null;
};

/** The `sessionStorage` key of the join answer (the fallback of an older deck). */
export const MEMBER_KEY = 'fd-member';

/** The heartbeat interval of an open session, in seconds (`PRESENCE_HEARTBEAT_S` of the proto). */
export const PRESENCE_HEARTBEAT_S = 20;

const ROLES: readonly ShareRole[] = ['viewer', 'collaborator', 'approver'];

/** The role text of the UI: `viewer`, `collaborator`, `approver`. */
export const roleLabel = (role: ShareRole): string => role;

/** One line for each role, for the share dialog. */
export const ROLE_TEXT: Readonly<Record<ShareRole, string>> = {
  viewer: 'sees the chat, the files and the changes',
  collaborator: 'also sends prompts and steers',
  approver: 'also answers approvals and questions',
};

/** What a role shows on a session: the composer, and the approval and question controls. */
export function roleAccess(role: ShareRole): { composer: boolean; approvals: boolean } {
  return {
    composer: role === 'collaborator',
    approvals: role === 'collaborator' || role === 'approver',
  };
}

const isRole = (v: unknown): v is ShareRole => ROLES.includes(v as ShareRole);

/** The name of a member actor: `member:<name>` is `<name>`; any other actor is `null`. */
export function memberName(actor: string): string | null {
  return actor.startsWith('member:') && actor.length > 7 ? actor.slice(7) : null;
}

/** The text for an actor in a list: the owner is "You" for the owner and "Owner" for a member. */
export function actorLabel(actor: string, self: string): string {
  if (actor === 'owner') return self === 'owner' ? 'You' : 'Owner';
  return memberName(actor) ?? actor;
}

/** The stored join answer, or `null` (blocked storage, nothing stored, a bad value). */
export function readStoredMember(storage: Pick<Storage, 'getItem'> | null): Member | null {
  try {
    const raw = storage?.getItem(MEMBER_KEY);
    if (raw === null || raw === undefined) return null;
    const v: unknown = JSON.parse(raw);
    if (typeof v !== 'object' || v === null) return null;
    const { role, display_name: name } = v as { role?: unknown; display_name?: unknown };
    if (!isRole(role) || typeof name !== 'string' || name === '') return null;
    return { role, name, scope: null };
  } catch {
    return null;
  }
}

/** Keeps the join answer for this tab. A blocked storage only loses the fallback. */
export function writeStoredMember(
  storage: Pick<Storage, 'setItem' | 'removeItem'> | null,
  answer: { role: ShareRole; display_name: string } | null,
): void {
  try {
    if (answer === null) storage?.removeItem(MEMBER_KEY);
    else storage?.setItem(MEMBER_KEY, JSON.stringify(answer));
  } catch {
    // The login answer of the deck is the first source; this is the fallback.
  }
}

/** The member of a login: the `auth.session` answer first, then the stored join answer. */
export function memberOf(
  login: AuthView | null,
  stored: Pick<Storage, 'getItem'> | null,
): Member | null {
  if (login === null) return null;
  if (isRole(login.role)) {
    return {
      role: login.role,
      name: login.display_name ?? memberName(login.actor) ?? login.actor,
      scope: login.scope ?? null,
    };
  }
  const name = memberName(login.actor);
  if (name === null) return null;
  const fallback = readStoredMember(stored);
  return fallback === null ? null : { ...fallback, name };
}

/** The pages that a guest never opens (the deck answers 403 for their calls). */
export const GUEST_HIDDEN_ROUTES: readonly string[] = [
  'new',
  'plans',
  'automations',
  'stations',
  'files',
  'activity',
  'compare',
  'artifacts',
  'artifact',
  'terminals',
  'shares',
  'spaces',
];
