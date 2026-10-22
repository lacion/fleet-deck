// ADR-008 §5.3 "Owner visibility": what a fleet-control coordinator answered. The deck sends the
// last 50 answers, the count of the last hour and the cap of the grant.

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

/** The value of the "Fleet control" fact: "2 answers in the last hour". */
export function answersText(lastHour: number): string {
  return `${lastHour} ${lastHour === 1 ? 'answer' : 'answers'} in the last hour`;
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
