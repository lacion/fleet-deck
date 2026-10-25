// B27 §3.7: the posture line at the top of a settings tab. It says which security controls are
// weaker than their secure default, and where each one is in effect (the file, the running
// process, the command line).
import type { Relaxed, RelaxedState } from '../gen';
import { Icon, TriangleAlert } from '../ui/icons';
import { NoticeIcon } from './NoticeIcon';

/** The words after an item, by where it is in effect. */
export const STATE_SUFFIX: Readonly<Record<RelaxedState, string>> = {
  active: '',
  after_restart: ' (after the next restart)',
  until_restart: ' (still in effect until the next restart)',
  command_line: ' (set on the command line)',
};

export const POSTURE_ABSENT =
  'This station does not report its security posture. Update the station to see it.';
export const POSTURE_SECURE = 'Security: every control is at its secure default.';
export const POSTURE_TITLE = 'Relaxed security';
export const POSTURE_RESTORE = 'Change a control below to restore its default.';

/** The sentence with the count of changed controls. */
export const countSentence = (n: number): string =>
  n === 1
    ? '1 control is changed from its secure default.'
    : `${n} controls are changed from their secure default.`;

/** `relaxed` is `undefined` when the station is older than the posture. */
export function PostureLine({
  relaxed,
  target,
}: {
  readonly relaxed: readonly Relaxed[] | undefined;
  /** What a restart applies the saved values to: the station of this tab, or the deck. */
  readonly target: 'station' | 'deck';
}) {
  if (relaxed === undefined) {
    return <p className="fd-note fd-set-posture">{POSTURE_ABSENT}</p>;
  }
  if (relaxed.length === 0) {
    return (
      <div className="fd-set-notice fd-set-posture" data-tone="ok" role="status">
        <NoticeIcon tone="ok" />
        <span>{POSTURE_SECURE}</span>
      </div>
    );
  }
  const restart = relaxed.some((r) => r.state === 'until_restart' || r.state === 'after_restart');
  return (
    <div className="fd-set-notice fd-set-posture" data-tone="error" role="status">
      <Icon of={TriangleAlert} size={14} />
      <div>
        <strong>{POSTURE_TITLE}</strong>
        <br />
        {countSentence(relaxed.length)}
        <ul>
          {relaxed.map((r) => (
            <li key={r.key}>
              {r.text}
              {STATE_SUFFIX[r.state]}
            </li>
          ))}
        </ul>
        {POSTURE_RESTORE}
        {restart ? ` Restart the ${target} to apply the saved values.` : null}
      </div>
    </div>
  );
}
