// ADR-008 §5.3 "Owner visibility": what a fleet-control coordinator answered. The deck sends the
// last 50 answers, the count of the last hour and the cap of the grant.
import { countText } from '../ui/format';

export type FleetAnswers = {
  fleet_control: boolean;
  answer_cap: number | null;
  answer_used: number;
  last_hour: number;
  answers: {
    at: number;
    target: string | null;
    result: string;
    detail: string | null;
    command_id: string | null;
  }[];
};

/** The shape check of the deck's answer: the two fields that the board needs to decide. */
export const isFleetAnswers = (v: unknown): v is FleetAnswers =>
  typeof v === 'object' &&
  v !== null &&
  typeof (v as Record<string, unknown>)['fleet_control'] === 'boolean' &&
  Array.isArray((v as Record<string, unknown>)['answers']);

/** The fact while the grant is on and the coordinator has not answered yet (plain text, no link). */
export const NO_ANSWERS_TEXT = 'On · no answers yet';

/**
 * The answers of the grant (its use, or the rows that the deck lists, whichever is larger): the
 * count of the link. `last_hour` counts the successful answers of the last hour only.
 */
export function answersTotal(data: FleetAnswers): number {
  return Math.max(data.answer_used, data.answers.length, data.last_hour);
}

/** The value of the "Fleet control" link: "12 answers · 2 in the last hour". */
export function answersText(total: number, lastHour: number): string {
  const recent =
    lastHour === 0 ? 'none in the last hour' : `${countText(lastHour)} in the last hour`;
  return `${countText(total)} ${total === 1 ? 'answer' : 'answers'} · ${recent}`;
}

/** The text of the result column. */
export function resultText(result: string): string {
  switch (result) {
    case 'ok':
      return 'Answered';
    case 'refused':
      return 'Refused';
    case 'failed':
      return 'Failed';
    default:
      return result;
  }
}
