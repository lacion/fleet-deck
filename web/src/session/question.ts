// M2 round 39 (DL-068): the answer form of a question request.
//
// A question asks for answers, not only for one of the provider's options.
// Each provider carries the answers in its own shape:
// - Codex `item/tool/requestUserInput` (response `rpc_reply`): option
//   `answer` with `updated_input = {answers: {<question id>: {answers: [..]}}}`;
// - Codex async questions (response `message`): option `answer` with the
//   answer text;
// - Claude `AskUserQuestion`: option `allow` with `updated_input` = the tool
//   input plus `answers: {<question text>: "<answer>"}`.
// The station treats these `updated_input` values as answers, not as input
// edits (DL-068). A request that matches none of these shapes keeps the
// plain option buttons.
import type { JsonValue, OpenRequest } from '../gen';

export type Question = {
  /** The key of the answer. */
  readonly id: string;
  readonly header: string;
  readonly text: string;
  readonly options: readonly string[];
  /** A free-text answer is allowed. */
  readonly other: boolean;
  readonly multi: boolean;
  readonly secret: boolean;
};

/** The decision fields of an answer, without the option id. */
export type Reply = { readonly updated_input: JsonValue | null; readonly text: string | null };

export type QuestionForm = {
  readonly questions: readonly Question[];
  /** The option that sends the answers. */
  readonly submit: string;
  /** The other options, shown as plain buttons (for example Claude `deny`). */
  readonly others: readonly string[];
  /** The decision for the answers, keyed by question id; one or more values each. */
  build(answers: Readonly<Record<string, readonly string[]>>): Reply;
};

type Obj = { readonly [key: string]: JsonValue };
const obj = (v: JsonValue | undefined): v is Obj =>
  typeof v === 'object' && v !== null && !Array.isArray(v);
const str = (v: JsonValue | undefined): string | null => (typeof v === 'string' ? v : null);

function labels(v: JsonValue | undefined): string[] {
  if (!Array.isArray(v)) return [];
  return v.flatMap((o) => {
    if (typeof o === 'string') return [o];
    const label = obj(o) ? str(o['label']) : null;
    return label === null ? [] : [label];
  });
}

const optionIds = (r: OpenRequest) => r.options.map((o) => o.option_id);

/** The answer form of `r`, or `null` when `r` is not a question that the board can answer. */
export function questionForm(r: OpenRequest): QuestionForm | null {
  if (r.request_kind !== 'question' || !obj(r.input ?? undefined)) return null;
  const input = r.input as Obj;
  const ids = optionIds(r);
  if (r.response === 'message' && ids.includes('answer')) {
    return {
      questions: [
        {
          id: 'answer',
          header: '',
          text: str(input['title']) ?? r.title,
          options: labels(input['options']),
          other: true,
          multi: false,
          secret: false,
        },
      ],
      submit: 'answer',
      others: [],
      build: (a) => ({ updated_input: null, text: (a['answer'] ?? []).join(', ') }),
    };
  }
  const list = Array.isArray(input['questions']) ? input['questions'].filter(obj) : [];
  if (list.length === 0) return null;
  const codex = list.every((q) => str(q['id']) !== null);
  if (codex && ids.includes('answer')) {
    return {
      questions: list.map((q) => ({
        id: str(q['id']) ?? '',
        header: str(q['header']) ?? '',
        text: str(q['question']) ?? '',
        options: labels(q['options']),
        other: q['isOther'] === true || labels(q['options']).length === 0,
        multi: false,
        secret: q['isSecret'] === true,
      })),
      submit: 'answer',
      others: ids.filter((id) => id !== 'answer'),
      build: (a) => ({
        updated_input: {
          answers: Object.fromEntries(
            list.map((q) => {
              const id = str(q['id']) ?? '';
              return [id, { answers: [...(a[id] ?? [])] }];
            }),
          ),
        },
        text: null,
      }),
    };
  }
  if (!codex && ids.includes('allow') && list.every((q) => str(q['question']) !== null)) {
    return {
      questions: list.map((q) => ({
        id: str(q['question']) ?? '',
        header: str(q['header']) ?? '',
        text: str(q['question']) ?? '',
        options: labels(q['options']),
        other: true,
        multi: q['multiSelect'] === true,
        secret: false,
      })),
      submit: 'allow',
      others: ids.filter((id) => id !== 'allow' && id !== 'allow_always'),
      build: (a) => ({
        updated_input: {
          ...input,
          answers: Object.fromEntries(
            list.map((q) => {
              const id = str(q['question']) ?? '';
              return [id, (a[id] ?? []).join(', ')];
            }),
          ),
        },
        text: null,
      }),
    };
  }
  return null;
}

/** True when every question has a non-empty answer. */
export function complete(
  form: QuestionForm,
  answers: Readonly<Record<string, readonly string[]>>,
): boolean {
  return form.questions.every((q) => (answers[q.id] ?? []).some((v) => v.trim() !== ''));
}
