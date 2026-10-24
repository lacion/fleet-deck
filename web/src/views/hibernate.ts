// R6.6 (DL-169): a hibernated session is an exited session that the station stopped on purpose.
// The board shows it as "Hibernated" and resumes it with the next input.
import type { DeckSession, ImageInput } from '../gen';
import type { InputKind, Outcome, SessionActions } from '../session/commands';

/** True while the session is stopped by hibernation; the next start clears the flag. */
export const isHibernated = (card: DeckSession): boolean =>
  card.summary.hibernated === true && card.summary.state === 'exited';

/** How often and how long `resumeThenSend` tries to send after the resume. */
export const RESUME_TRIES = 20;
export const RESUME_WAIT_MS = 500;

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/**
 * Resumes the session, then sends the text as a prompt. A resumed process takes input
 * only when it is live, so the station refuses the first tries; the send is repeated
 * while the station refuses it, until it is accepted or the tries are used. Any other
 * failure (a lost link, an unknown delivery) ends the loop at once.
 */
export async function resumeThenSend(
  actions: SessionActions,
  _kind: InputKind,
  text: string,
  images: readonly ImageInput[],
  wait: (ms: number) => Promise<void> = sleep,
): Promise<Outcome> {
  const resumed = await actions.resume();
  if (!resumed.ok) return { ok: false, text: `Not resumed: ${resumed.text}` };
  let last: Outcome = { ok: false, text: 'The session did not start in time.' };
  for (let n = 0; n < RESUME_TRIES; n += 1) {
    last = await actions.send('prompt', text, images);
    if (last.ok) return last;
    if (!last.text.startsWith('Refused:')) return last;
    await wait(RESUME_WAIT_MS);
  }
  return {
    ok: false,
    text: `The session resumed, but did not take the prompt: ${last.text}`,
  };
}
