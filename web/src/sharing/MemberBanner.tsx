// R11.4 (DL-166): the slim banner at the top of a member's board.
import type { Member } from './member';
import { roleLabel } from './member';

export function MemberBanner({
  member,
  onLeave,
}: {
  readonly member: Member;
  readonly onLeave: (() => void) | undefined;
}) {
  return (
    <div className="fd-member-banner" role="note" aria-label="Shared with you">
      <span className="fd-member-banner-text">
        Shared with you as {roleLabel(member.role)} by the owner · {member.name}
      </span>
      {onLeave === undefined ? null : (
        <button type="button" className="fd-ghost fd-sm" onClick={onLeave}>
          Leave
        </button>
      )}
    </div>
  );
}
