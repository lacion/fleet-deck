// M2 round 39 (DL-068): the answer form of one question request.
import { useState } from 'react';
import type { OpenRequest } from '../gen';
import { complete, type QuestionForm, type Reply } from './question';

type Props = {
  readonly request: OpenRequest;
  readonly form: QuestionForm;
  readonly enabled: boolean;
  readonly busy: boolean;
  readonly onAnswer: (option: string, reply: Reply | null) => void;
};

export function QuestionAnswer({ request, form, enabled, busy, onAnswer }: Props) {
  const [chosen, setChosen] = useState<Record<string, readonly string[]>>({});
  const [typed, setTyped] = useState<Record<string, string>>({});
  // A typed answer replaces the choice of a single-answer question and adds
  // to the choices of a multi-answer question.
  const answers: Record<string, readonly string[]> = Object.fromEntries(
    form.questions.map((q) => {
      const text = (typed[q.id] ?? '').trim();
      const picked = chosen[q.id] ?? [];
      if (text === '') return [q.id, picked];
      return [q.id, q.multi ? [...picked, text] : [text]];
    }),
  );
  const pick = (id: string, option: string, multi: boolean, on: boolean) =>
    setChosen((c) => {
      const now = c[id] ?? [];
      const next = multi ? (on ? [...now, option] : now.filter((o) => o !== option)) : [option];
      return { ...c, [id]: next };
    });
  const label = (id: string) => request.options.find((o) => o.option_id === id)?.label ?? id;
  return (
    <form
      className="fd-question"
      aria-label={`Answer: ${request.title}`}
      onSubmit={(e) => {
        e.preventDefault();
        onAnswer(form.submit, form.build(answers));
      }}
    >
      {form.questions.map((q, n) => (
        <fieldset key={q.id} disabled={!enabled || busy}>
          <legend>{q.header ? `${q.header}: ${q.text}` : q.text}</legend>
          {q.options.map((o) => (
            <label key={o} className="fd-opt">
              <input
                type={q.multi ? 'checkbox' : 'radio'}
                name={`${request.request_id}-${n}`}
                value={o}
                checked={(chosen[q.id] ?? []).includes(o)}
                onChange={(e) => pick(q.id, o, q.multi, e.currentTarget.checked)}
              />
              {o}
            </label>
          ))}
          {q.other ? (
            <label className="fd-opt-other">
              {q.options.length > 0 ? 'Other answer' : 'Your answer'}
              <input
                type={q.secret ? 'password' : 'text'}
                value={typed[q.id] ?? ''}
                onChange={(e) => {
                  const value = e.currentTarget.value;
                  setTyped((t) => ({ ...t, [q.id]: value }));
                }}
              />
            </label>
          ) : null}
        </fieldset>
      ))}
      <div className="fd-options">
        <button
          type="submit"
          className="fd-primary"
          disabled={!enabled || busy || !complete(form, answers)}
        >
          Send answer
        </button>
        {form.others.map((id) => (
          <button
            key={id}
            type="button"
            disabled={!enabled || busy}
            onClick={() => onAnswer(id, null)}
          >
            {label(id)}
          </button>
        ))}
      </div>
    </form>
  );
}
