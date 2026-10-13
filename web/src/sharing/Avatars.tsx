// R11.5 (DL-166): who is looking at the session: round initials in the session header.
import { actorLabel } from './member';
import { initial, MAX_AVATARS } from './people';

/** The names of `who` for `self`, each person once, in the order of the deck. */
export function presentNames(who: readonly string[], self: string): string[] {
  return [...new Set(who)].map((actor) => actorLabel(actor, self));
}

/** Up to five round initials with the name as the tooltip, and "+n" for the rest. */
export function Avatars({ who, self }: { readonly who: readonly string[]; readonly self: string }) {
  const names = presentNames(who, self);
  if (names.length === 0) return null;
  const shown = names.slice(0, MAX_AVATARS);
  const more = names.length - shown.length;
  return (
    <ul className="fd-avatars" aria-label="People here">
      {shown.map((name, n) => (
        <li key={`${name}-${n}`} className="fd-avatar" title={name} role="img" aria-label={name}>
          {initial(name)}
        </li>
      ))}
      {more > 0 ? (
        <li className="fd-avatar-more" aria-label={`${more} more`}>{`+${more}`}</li>
      ) : null}
    </ul>
  );
}
