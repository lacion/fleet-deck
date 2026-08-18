import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  human,
  basename,
  questionView,
  sessionsById,
  spawnTermable,
  TURN_BOUNDARY_HINT,
} from '../util.ts';
import { renderMarkdown, planTitle } from '../markdown.ts';
import { answerQuestion, dismissQuestion, reasonOf } from '../api.ts';
import { registerQuestion, type QuestionHandle } from '../qbus.ts';
import { ErrorBoundary } from './ErrorBoundary.tsx';
import type { QuestionEntry, SessionEntry } from '../../../contracts/index.ts';

// The fixed right rail: NEEDS YOU. Global (never repo-filtered — the human's
// attention is the one resource that isn't per-project), badged by repo.

// questionView() owns the shape of a rendered question; the body components
// read that shape structurally rather than re-deriving it.
type QView = ReturnType<typeof questionView>;
type ChoiceQ = NonNullable<QView['questions']>[number];
type ChoiceOpt = NonNullable<ChoiceQ['options']>[number];
// The answer body is provider-shaped and flows straight into answerQuestion(id,
// body: unknown) — so it stays unknown here too, exactly as the api surface has it.
type OnAnswer = (body: unknown, label: string) => void;
// Imperative handle a body fills in so App's 1-9 / Enter keys reach it.
interface BodyApi {
  choose?: (n: number) => void;
  focusInput?: () => void;
}
type BindKeys = (api: BodyApi | null) => void;
// A status/result line: a css class plus its text.
interface Note {
  cls: string;
  text: string;
}
// The defensively-read corners of a question payload (contract types it
// `unknown`; each field stays optional so every `?.` below is honest).
interface QPayload {
  tool_name?: string;
  tool_input?: { plan?: string };
  rearmed?: boolean;
}
// One elicitation schema property, as the form reads it.
interface ElicitProp {
  type?: string;
  enum?: (string | number)[];
  description?: string;
}

interface CountdownRingProps {
  q: QuestionEntry;
  now: number;
}

function CountdownRing({ q, now }: CountdownRingProps) {
  if (!q.expires_at) return null;
  const total = Math.max(1, q.expires_at - (q.created_at || q.expires_at - 50000));
  const secs = Math.max(0, Math.ceil((q.expires_at - now) / 1000));
  const frac = Math.max(0, Math.min(1, (q.expires_at - now) / total));
  const color = secs <= 12 ? 'var(--hazard)' : 'var(--act)';
  // The ring fits two glyphs: raw seconds under 100, whole minutes above —
  // the 0.21.1 default window is 600 s and a 26 px ring cannot print "600".
  const label = secs > 99 ? `${Math.ceil(secs / 60)}m` : `${secs}`;
  const tip =
    secs > 99
      ? `~${Math.ceil(secs / 60)} min until this falls back to the terminal`
      : `${secs}s until this falls back to the terminal`;
  return (
    <span className="fd-ring" title={tip} aria-label={tip}>
      <svg width="26" height="26" viewBox="0 0 26 26" aria-hidden="true">
        <circle cx="13" cy="13" r="10.5" fill="none" stroke="var(--border)" strokeWidth="2.5" />
        <circle
          className="arc"
          cx="13"
          cy="13"
          r="10.5"
          fill="none"
          stroke={color}
          strokeWidth="2.5"
          strokeLinecap="round"
          strokeDasharray="66"
          strokeDashoffset={66 * (1 - frac)}
        />
      </svg>
      <span className="secs" style={{ color }}>
        {label}
      </span>
    </span>
  );
}

interface PermissionBodyProps {
  view: QView;
  busy: boolean;
  onAnswer: OnAnswer;
}

function AllowDenyButtons({
  busy,
  allowLabel,
  answerLabel,
  onAnswer,
}: {
  busy: boolean;
  allowLabel: string;
  answerLabel: string;
  onAnswer: OnAnswer;
}) {
  return (
    <div className="fd-btnrow">
      <button
        type="button"
        className="fd-allow"
        disabled={busy}
        onClick={() => {
          onAnswer({ behavior: 'allow' }, answerLabel);
        }}
      >
        {allowLabel} <span className="k">y</span>
      </button>
      <button
        type="button"
        className="fd-deny"
        disabled={busy}
        onClick={() => {
          onAnswer({ behavior: 'deny' }, 'deny');
        }}
      >
        Deny <span className="k">n</span>
      </button>
    </div>
  );
}

function PermissionBody({ view, busy, onAnswer }: PermissionBodyProps) {
  return (
    <>
      {view.command && !view.diff && <div className="cmd">{view.command}</div>}
      {view.diff && (
        <div className="cmd">
          <span style={{ color: 'var(--dim)' }}>
            {view.command}
            {'\n'}
          </span>
          {view.diff.map((l, i) => (
            <span key={i} className={l.kind}>
              {l.text}
              {'\n'}
            </span>
          ))}
        </div>
      )}
      <AllowDenyButtons busy={busy} allowLabel="Allow" answerLabel="allow" onAnswer={onAnswer} />
    </>
  );
}

interface PlanBodyProps {
  q: QuestionEntry;
  busy: boolean;
  onAnswer: OnAnswer;
}

// v1.3 — an ExitPlanMode permission renders as a PLAN card: the rendered
// plan (tiny local markdown, everything escaped), then Approve / Capture &
// release / Deny. Approve/Deny keep the .fd-allow/.fd-deny classes so the
// y/n keys keep working; capture is the board-only pseudo-behavior (the
// daemon denies the hook AND mails the planner to stop without executing).
function PlanBody({ q, busy, onAnswer }: PlanBodyProps) {
  const payload = q.payload as QPayload | null | undefined;
  const md = payload?.tool_input?.plan ?? '';
  // M-P7 — parse the plan markdown once per body text, not on every 1 s render.
  // M-P2 — a resolved snapshot may omit the plan body; render a placeholder
  // rather than an empty box (never assume the body is present).
  const html = useMemo(() => renderMarkdown(md), [md]);
  return (
    <>
      {md.trim() ? (
        <div className="fd-md rail" dangerouslySetInnerHTML={{ __html: html }} />
      ) : (
        <div className="fd-md rail">
          <em>plan text not included in this snapshot</em>
        </div>
      )}
      <AllowDenyButtons
        busy={busy}
        allowLabel="Approve"
        answerLabel="approve"
        onAnswer={onAnswer}
      />
      <button
        type="button"
        className="fd-capture"
        disabled={busy}
        title="store the plan; the planner stops without executing"
        onClick={() => {
          onAnswer({ behavior: 'capture' }, 'capture & release');
        }}
      >
        Capture &amp; release
      </button>
    </>
  );
}

interface ChoiceBodyProps {
  view: QView;
  busy: boolean;
  onAnswer: OnAnswer;
  bindKeys?: BindKeys;
}

function ChoiceBody({ view, busy, onAnswer, bindKeys }: ChoiceBodyProps) {
  const questions = view.questions ?? [];
  const [picked, setPicked] = useState<Record<string, string | string[] | undefined>>({}); // question text -> label | [labels]
  const multi = questions.length > 1 || questions.some((x) => x.multiSelect);
  let optionN = 0;
  const optionOffsets = questions.map((question) => {
    const offset = optionN;
    optionN += question.options?.length ?? 0;
    return offset;
  });

  const pick = (question: ChoiceQ, opt: ChoiceOpt) => {
    if (!multi) {
      onAnswer({ answers: { [question.question]: opt.label } }, opt.label);
      return;
    }
    setPicked((prev) => {
      const cur = prev[question.question];
      if (question.multiSelect) {
        const arr = Array.isArray(cur) ? [...cur] : [];
        const i = arr.indexOf(opt.label);
        if (i === -1) arr.push(opt.label);
        else arr.splice(i, 1);
        return { ...prev, [question.question]: arr };
      }
      return { ...prev, [question.question]: cur === opt.label ? undefined : opt.label };
    });
  };

  // M-F6 — expose "act on the n-th option" so App's 1-9 keys reach this card
  // through the registry, exactly as clicking the n-th option button would
  // (options are numbered across all questions in render order).
  useEffect(() => {
    bindKeys?.({
      choose: (n) => {
        if (busy) return;
        let idx = n - 1;
        for (const question of questions) {
          const opts = question.options ?? [];
          if (idx < opts.length) {
            const opt = opts[idx];
            if (opt) pick(question, opt);
            return;
          }
          idx -= opts.length;
        }
      },
    });
    return () => bindKeys?.(null);
    // pick closes over `picked`/`multi`; re-bind when those (or busy) change
  }, [view, picked, busy, bindKeys]);

  const complete = questions.every((x) => {
    const v = picked[x.question];
    return Array.isArray(v) ? v.length > 0 : !!v;
  });
  const submit = () => {
    const answers: Record<string, string | string[] | undefined> = {};
    for (const x of questions) answers[x.question] = picked[x.question];
    onAnswer({ answers }, 'answers sent');
  };

  return (
    <>
      {questions.map((question, qi) => (
        <div key={qi} style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
          {question.header && (
            <span className="fd-choicehead">{question.header.toUpperCase()}</span>
          )}
          {qi > 0 && (
            <div className="title" style={{ fontWeight: 500 }}>
              {question.question}
            </div>
          )}
          <div className="fd-opts">
            {(question.options ?? []).map((opt, i) => {
              const shortcut = (optionOffsets[qi] ?? 0) + i + 1;
              const cur = picked[question.question];
              const isPicked = Array.isArray(cur) ? cur.includes(opt.label) : cur === opt.label;
              return (
                <button
                  key={i}
                  type="button"
                  className={`fd-opt${isPicked ? ' picked' : ''}`}
                  disabled={busy}
                  onClick={() => {
                    pick(question, opt);
                  }}
                >
                  <span className="n">{shortcut <= 9 ? shortcut : '·'}</span>
                  <span className="body">
                    <span className="l">{opt.label}</span>
                    {opt.description && <span className="d">{opt.description}</span>}
                  </span>
                </button>
              );
            })}
          </div>
          {question.multiSelect && (
            <span className="micro">multi-select — pick any, then send</span>
          )}
        </div>
      ))}
      {multi && (
        <div className="fd-freerow">
          <span className="fd-spacer" />
          <button type="button" className="fd-send" disabled={busy || !complete} onClick={submit}>
            Send
          </button>
        </div>
      )}
    </>
  );
}

interface FreeformBodyProps {
  offline: boolean;
  busy: boolean;
  onAnswer: OnAnswer;
  bindKeys?: BindKeys;
}

function FreeformBody({ offline, busy, onAnswer, bindKeys }: FreeformBodyProps) {
  const [draft, setDraft] = useState('');
  const taRef = useRef<HTMLTextAreaElement | null>(null);
  const send = () => {
    if (!busy && draft.trim()) onAnswer({ text: draft.trim() }, 'sent');
  };
  // M-F6 — App's Enter-on-a-selected-freeform focuses the textarea through this
  // handle instead of a document.querySelector('textarea').
  useEffect(() => {
    bindKeys?.({ focusInput: () => taRef.current?.focus() });
    return () => bindKeys?.(null);
  }, [bindKeys]);
  return (
    <>
      <div className="fd-freerow">
        <textarea
          ref={taRef}
          className="fd-input"
          rows={2}
          placeholder="Type an answer…"
          value={draft}
          onChange={(e) => {
            setDraft(e.target.value);
          }}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.shiftKey) {
              e.preventDefault();
              send();
            }
          }}
        />
        <button type="button" className="fd-send" disabled={busy || !draft.trim()} onClick={send}>
          Send
        </button>
      </div>
      <span className="micro">
        {offline
          ? 'session is offline — your answer delivers on resume'
          : `delivered at ${TURN_BOUNDARY_HINT}`}
      </span>
    </>
  );
}

interface ElicitationBodyProps {
  view: QView;
  busy: boolean;
  onAnswer: OnAnswer;
}

// Schema-driven basic form for elicitation, with a raw-JSON fallback.
function ElicitationBody({ view, busy, onAnswer }: ElicitationBodyProps) {
  const props: [string, ElicitProp | null][] =
    view.schema?.properties && typeof view.schema.properties === 'object'
      ? Object.entries(view.schema.properties as Record<string, ElicitProp | null>)
      : [];
  const simple =
    props.length > 0 &&
    props.every(
      ([, def]) =>
        def && (def.enum ?? ['string', 'number', 'integer', 'boolean'].includes(def.type ?? '')),
    );
  const [useJson, setUseJson] = useState(!simple);
  const [fields, setFields] = useState<Record<string, string | boolean>>({});
  const [jsonDraft, setJsonDraft] = useState('{\n}');
  const [jsonErr, setJsonErr] = useState<string | null>(null);
  const required = new Set<string>((view.schema?.required as string[] | undefined) ?? []);

  const accept = () => {
    if (useJson) {
      try {
        const content: unknown = JSON.parse(jsonDraft);
        onAnswer({ action: 'accept', content }, 'accepted');
      } catch {
        setJsonErr('not valid JSON');
      }
      return;
    }
    const content: Record<string, unknown> = {};
    for (const [name, def] of props) {
      const v = fields[name];
      if (v === undefined || v === '') continue;
      content[name] =
        def?.type === 'number' || def?.type === 'integer'
          ? Number(v)
          : def?.type === 'boolean'
            ? !!v
            : v;
    }
    onAnswer({ action: 'accept', content }, 'accepted');
  };
  const missingRequired =
    !useJson &&
    [...required].some((r) => {
      const v = fields[r];
      return v === undefined || v === '';
    });

  return (
    <>
      {!useJson ? (
        <div className="fd-form">
          {props.map(([name, def]) => (
            <div className="frow" key={name}>
              <span className="fl" title={(def?.description ?? '') || name}>
                {name}
                {required.has(name) ? ' *' : ''}
              </span>
              {def?.enum ? (
                <select
                  className="fd-input"
                  value={String(fields[name] ?? '')}
                  onChange={(e) => {
                    setFields({ ...fields, [name]: e.target.value });
                  }}
                >
                  <option value="" disabled>
                    choose…
                  </option>
                  {def.enum.map((v) => (
                    <option key={String(v)} value={v}>
                      {String(v)}
                    </option>
                  ))}
                </select>
              ) : def?.type === 'boolean' ? (
                <input
                  type="checkbox"
                  checked={!!fields[name]}
                  onChange={(e) => {
                    setFields({ ...fields, [name]: e.target.checked });
                  }}
                />
              ) : (
                <input
                  className="fd-input"
                  type={def?.type === 'number' || def?.type === 'integer' ? 'number' : 'text'}
                  placeholder={(def?.description ?? '') || (def?.type ?? '') || ''}
                  value={String(fields[name] ?? '')}
                  onChange={(e) => {
                    setFields({ ...fields, [name]: e.target.value });
                  }}
                />
              )}
            </div>
          ))}
        </div>
      ) : (
        <>
          <textarea
            className="fd-input"
            rows={4}
            value={jsonDraft}
            onChange={(e) => {
              setJsonDraft(e.target.value);
              setJsonErr(null);
            }}
            spellCheck={false}
          />
          {jsonErr && <span className="status hazard">{jsonErr}</span>}
        </>
      )}
      <div className="fd-btnrow">
        <button
          type="button"
          className="fd-allow"
          disabled={busy || missingRequired}
          onClick={accept}
        >
          Accept
        </button>
        <button
          type="button"
          className="fd-deny"
          disabled={busy}
          onClick={() => {
            onAnswer({ action: 'decline' }, 'declined');
          }}
        >
          Decline
        </button>
      </div>
      {simple && (
        <button
          type="button"
          className="micro"
          style={{
            background: 'none',
            border: 'none',
            color: 'var(--faint)',
            cursor: 'pointer',
            textAlign: 'left',
            padding: 0,
          }}
          onClick={() => {
            setUseJson(!useJson);
          }}
        >
          {useJson ? '← back to the form' : 'answer as raw JSON instead'}
        </button>
      )}
    </>
  );
}

const KIND_LABEL: Record<string, string> = {
  permission: 'PERMISSION',
  choice: 'CHOICE',
  freeform: 'FREE-TEXT',
  elicitation: 'FORM',
};

function statusLine(q: QuestionEntry, session: SessionEntry | undefined): Note | null {
  const payload = q.payload as QPayload | null | undefined;
  if (q.status === 'answered') {
    if (q.kind === 'freeform') {
      const offline = session?.col === 'offline';
      return {
        cls: 'ok',
        text: offline
          ? '✓ queued — delivers on resume'
          : `✓ queued — delivers at ${TURN_BOUNDARY_HINT}`,
      };
    }
    // 2.1 — a re-armed card never had a socket to answer into; its "answer"
    // is mail, so the resolved line keeps the same honesty as the live one.
    if (payload?.rearmed)
      return { cls: 'ok', text: `✓ queued — delivers at ${TURN_BOUNDARY_HINT}` };
    return { cls: 'ok', text: '✓ answered from the board' };
  }
  if (q.status === 'expired') return { cls: '', text: '⏱ expired — the terminal owns this one' };
  return null;
}

// 2.1 focus-terminal — an expired card is dead for answering: the hook already
// failed open and the agent is parked on its NATIVE prompt in a pane somewhere.
// The one recovery the board can offer is navigation, not an answer — a ghost
// button (deliberately not the fd-allow/fd-deny language of a live decision)
// that opens the floating terminal onto the owning session's pane, where the
// prompt waits. Only when the session is termable: a plain `claude` in the
// user's own terminal has no board-owned pane to open, so the button would be
// a dead end; there the status line above already says where the decision
// lives. onOpenTerm is optional — without it (or without the session row) the
// card degrades to exactly the pre-2.1 render.
interface DeadCardNavProps {
  q: QuestionEntry;
  session: SessionEntry | undefined;
  onOpenTerm?: ((session: SessionEntry) => void) | undefined;
}

function DeadCardNav({ session, onOpenTerm }: DeadCardNavProps) {
  if (!onOpenTerm || !session || !spawnTermable(session)) return null;
  return (
    <button
      type="button"
      className="fd-ghostbtn fd-openterm"
      title="open a live terminal onto this session's pane — the agent's own prompt is waiting there"
      onClick={(e) => {
        e.stopPropagation();
        onOpenTerm(session);
      }}
    >
      ▣ Open terminal
    </button>
  );
}

// Both a normal card and its ErrorBoundary fallback dismiss through the same
// endpoint. Keep their single-flight latch and success/failure lifecycle here;
// each surface only supplies the note shape it renders.
function useQuestionDismiss(
  questionId: string,
  onDismissed: ((id: string) => void) | undefined,
  setError: (reason: string | null) => void,
) {
  const [dismissing, setDismissing] = useState(false);
  const dismissingRef = useRef(false);

  const dismiss = useCallback(async () => {
    if (dismissingRef.current) return;
    dismissingRef.current = true;
    setDismissing(true);
    setError(null);
    let removed = false;
    try {
      const res = await dismissQuestion(questionId);
      if (res.ok && res.json?.ok !== false) {
        removed = true;
        onDismissed?.(questionId);
        return;
      }
      setError(reasonOf(res, `dismiss failed (${res.status})`));
    } catch {
      setError('dismiss failed — daemon unreachable');
    } finally {
      if (!removed) {
        dismissingRef.current = false;
        setDismissing(false);
      }
    }
  }, [questionId, onDismissed, setError]);

  return { dismissing, dismiss };
}

interface QuestionCardProps {
  q: QuestionEntry;
  session: SessionEntry | undefined;
  now: number;
  selected: boolean;
  onSelect: () => void;
  onDismissed?: (id: string) => void;
  onOpenTerm?: ((session: SessionEntry) => void) | undefined;
}

function QuestionCard({
  q,
  session,
  now,
  selected,
  onSelect,
  onDismissed,
  onOpenTerm,
}: QuestionCardProps) {
  const [busy, setBusy] = useState(false);
  const answeringRef = useRef(false);
  const answeredRef = useRef(false);
  const [answered, setAnswered] = useState(false);
  const [note, setNote] = useState<Note | null>(null); // transient result of an answer POST
  // v1.8 — dismiss: the question you already answered in the terminal. Low
  // risk (the daemon expires it and sends the session NOTHING), so no confirm
  // — just an in-flight lock and an honest failure line if the daemon says no.
  const { dismissing, dismiss: doDismiss } = useQuestionDismiss(q.id, onDismissed, (reason) => {
    setNote(reason ? { cls: 'hazard', text: reason } : null);
  });
  const view = questionView(q);
  const payload = q.payload as QPayload | null | undefined;
  // v1.3 — ExitPlanMode permissions are PLAN cards (they also carry plan_id)
  const isPlan = q.kind === 'permission' && payload?.tool_name === 'ExitPlanMode';
  const pending = q.status === 'pending';
  // 2.1 — a re-armed card: the hold window expired and the daemon re-raised
  // the still-unanswered question. There is NO parked socket behind it, so an
  // answer goes as mail at the next turn boundary, never into the hook. The
  // copy must keep that distinction honest — a live hold's buttons unblock
  // the agent NOW, these don't.
  const rearmed = payload?.rearmed === true;
  const holdKind = q.kind !== 'freeform';
  const holdLost = pending && holdKind && !q.held;
  const done = !pending;
  const repoName =
    (session?.repo_name ?? '') || (session?.repo_id ? basename(session.repo_id) : null);
  const offline = session?.col === 'offline';
  // M-P7 — the plan title is derived from the plan body; memo it so a 1 s
  // re-render doesn't re-scan the markdown. M-P2 — planTitle already degrades
  // to 'untitled plan' when the body is absent.
  const planMd = isPlan ? (payload.tool_input?.plan ?? '') : '';
  const title = useMemo(
    () => (isPlan ? planTitle(planMd) : view.title),
    [isPlan, planMd, view.title],
  );

  const onAnswer = useCallback(
    async (body: unknown, label: string) => {
      if (answeringRef.current || answeredRef.current) return;
      answeringRef.current = true;
      setBusy(true);
      setNote(null);
      try {
        const res = await answerQuestion(q.id, body);
        if (res.ok) {
          // Latch locally as soon as the daemon accepts. The snapshot can trail
          // the POST by a frame; re-enabling controls in that gap can answer one
          // held tool call twice.
          answeredRef.current = true;
          setAnswered(true);
          setNote({
            cls: q.kind === 'freeform' || rearmed ? 'act' : 'ok',
            text:
              q.kind === 'freeform' || rearmed
                ? offline
                  ? `→ ${label} · queued — delivers on resume`
                  : `→ ${label} · queued — delivers at ${TURN_BOUNDARY_HINT}`
                : `→ ${label} — sent to agent`,
          });
        } else {
          setNote({ cls: 'hazard', text: reasonOf(res, `answer failed (${res.status})`) });
        }
      } catch {
        setNote({ cls: 'hazard', text: 'answer failed — daemon unreachable' });
      } finally {
        answeringRef.current = false;
        setBusy(false);
      }
    },
    [q.id, q.kind, offline, rearmed],
  );

  const resolved = statusLine(q, session);
  const interactive = pending && !holdLost && !answered;

  // M-F6 — the imperative handle App's hotkeys reach this card by. Registered
  // per id; the body components fill in `bodyApi` (choose / focusInput) so no
  // CSS class is load-bearing for the keyboard path.
  const cardRef = useRef<HTMLDivElement | null>(null);
  const bodyApi = useRef<BodyApi>({});
  const bindKeys = useCallback<BindKeys>((api) => {
    bodyApi.current = api ?? {};
  }, []);
  useEffect(() => {
    const handle: QuestionHandle = {
      allow: () => {
        if (interactive && !busy)
          void onAnswer({ behavior: 'allow' }, isPlan ? 'approve' : 'allow');
      },
      deny: () => {
        if (interactive && !busy) void onAnswer({ behavior: 'deny' }, 'deny');
      },
      choose: (n) => {
        if (interactive && !busy) bodyApi.current.choose?.(n);
      },
      focusInput: () => {
        bodyApi.current.focusInput?.();
      },
      scrollIntoView: () => {
        cardRef.current?.scrollIntoView({ block: 'nearest' });
      },
    };
    return registerQuestion(q.id, handle);
  }, [q.id, interactive, busy, isPlan, onAnswer]);

  return (
    <div
      ref={cardRef}
      className={`fd-q${selected && pending ? ' sel' : ''}${done ? ' done' : ''}`}
      aria-label={`${(q.callsign ?? '') || q.session_id}: ${title}`}
      onClick={() => {
        if (pending) onSelect();
      }}
    >
      <div className="row1">
        <span className="callsign">{(q.callsign ?? '') || q.session_id}</span>
        <span className={`fd-kind ${isPlan ? 'plan' : q.kind}`}>
          {isPlan ? 'PLAN' : (KIND_LABEL[q.kind] ?? q.kind.toUpperCase())}
        </span>
        {rearmed && pending && (
          <span
            className="fd-kind rearmed"
            title="the live answer window expired — the agent is parked on its own terminal prompt; answering here sends a message instead"
          >
            RE-ARMED
          </span>
        )}
        {repoName && <span className="repo">{repoName}</span>}
        <span className="fd-spacer" />
        <span className="age">{human(now - (q.created_at || now))}</span>
        {pending && !holdLost && <CountdownRing q={q} now={now} />}
        {pending && (
          <button
            type="button"
            className="fd-qdismiss"
            aria-label="Dismiss this question"
            title="dismiss — you already handled this in the terminal"
            disabled={dismissing}
            onClick={(e) => {
              e.stopPropagation();
              void doDismiss();
            }}
          >
            ✕
          </button>
        )}
      </div>
      <div className="title">{title}</div>
      {rearmed && pending && (
        <div className="status act">
          ⏱ board window expired — the agent is parked on its own terminal prompt; answering sends a
          message, delivered at the next turn boundary
        </div>
      )}
      {holdLost && (
        <div className="status hazard">⚠ hold lost (daemon restarted) — decide in the terminal</div>
      )}
      {interactive && isPlan && (
        <PlanBody q={q} busy={busy} onAnswer={(body, label) => void onAnswer(body, label)} />
      )}
      {interactive && q.kind === 'permission' && !isPlan && (
        <PermissionBody
          view={view}
          busy={busy}
          onAnswer={(body, label) => void onAnswer(body, label)}
        />
      )}
      {interactive && q.kind === 'choice' && (
        <ChoiceBody
          view={view}
          busy={busy}
          onAnswer={(body, label) => void onAnswer(body, label)}
          bindKeys={bindKeys}
        />
      )}
      {interactive && q.kind === 'freeform' && (
        <FreeformBody
          offline={offline}
          busy={busy}
          onAnswer={(body, label) => void onAnswer(body, label)}
          bindKeys={bindKeys}
        />
      )}
      {interactive && q.kind === 'elicitation' && (
        <ElicitationBody
          view={view}
          busy={busy}
          onAnswer={(body, label) => void onAnswer(body, label)}
        />
      )}
      {note && pending && (
        <div className={`status ${note.cls}`} role={note.cls === 'hazard' ? 'alert' : 'status'}>
          {note.text}
        </div>
      )}
      {resolved && (
        <div className={`status ${resolved.cls}`} role="status">
          {resolved.text}
        </div>
      )}
      {done && <DeadCardNav q={q} session={session} onOpenTerm={onOpenTerm} />}
    </div>
  );
}

// Fallback for a QuestionCard whose render threw — almost always a malformed,
// server-persisted hook payload. It degrades that one row to an honest,
// dismissible card so the poison can be cleared (it re-throws on every reload
// otherwise), while the rest of the rail and the board keep working.
function PoisonCard({ q, onDismissed }: { q: QuestionEntry; onDismissed?: (id: string) => void }) {
  const [note, setNote] = useState<string | null>(null);
  const { dismissing, dismiss: doDismiss } = useQuestionDismiss(q.id, onDismissed, (reason) => {
    setNote(reason);
  });
  return (
    <div className="fd-q">
      <div className="row1">
        {/* This card IS the ErrorBoundary fallback — a throw here escapes the
            boundary and white-screens the board it exists to save. Coerce every
            field: an object callsign would otherwise be an invalid React child. */}
        <span className="callsign">{String(q.callsign ?? '') || String(q.session_id ?? '')}</span>
        <span className="fd-kind">BROKEN</span>
        <span className="fd-spacer" />
        <button
          type="button"
          className="fd-qdismiss"
          aria-label="Dismiss this broken question"
          title="this card's payload couldn't be displayed — dismiss it"
          disabled={dismissing}
          onClick={() => void doDismiss()}
        >
          ✕
        </button>
      </div>
      <div className="title">This question couldn't be displayed</div>
      <div className="status hazard">
        ⚠ a malformed payload broke this card — the rest of the board is fine. Dismiss it to clear
        the row.
      </div>
      {note && (
        <div className="status hazard" role="alert">
          {note}
        </div>
      )}
    </div>
  );
}

interface InboxProps {
  questions: QuestionEntry[];
  sessions: SessionEntry[];
  now: number;
  selQ: string | null;
  onSelect: (id: string) => void;
  onOpenTerm?: ((session: SessionEntry) => void) | undefined;
}

export default function Inbox({
  questions,
  sessions,
  now,
  selQ,
  onSelect,
  onOpenTerm,
}: InboxProps) {
  const byId = sessionsById(sessions);
  // v1.8 — dismissed ids stay hidden in this tab. The daemon expires the
  // question, but an expired question still rides the snapshot for a while
  // (F3: pending + the last few resolved) and would come back as a faded
  // "⏱ expired" card — which is exactly the stale clutter being cleared.
  const [dismissed, setDismissed] = useState<Set<string>>(() => new Set());
  const live = questions.filter((q) => !dismissed.has(q.id));
  const pending = live.filter((q) => q.status === 'pending');
  const resolved = live.filter((q) => q.status !== 'pending');
  const ordered = [...pending, ...resolved];

  return (
    <div className="fd-inbox">
      <div className="fd-inboxhead">
        <span className="lbl">NEEDS YOU</span>
        {pending.length > 0 && <span className="count">{pending.length}</span>}
        <span className="fd-spacer" />
        <span className="keys">j/k · 1-9 · y/n · ⏎</span>
      </div>
      <div className="fd-inboxlist" id="fd-inboxlist">
        {ordered.length === 0 && (
          <div className="fd-allclear" role="status">
            <div className="ring">✓</div>
            <div className="t1">ALL CLEAR</div>
            <div className="t2">No one is waiting on you.</div>
          </div>
        )}
        {ordered.map((q) => (
          <ErrorBoundary
            key={q.id}
            fallback={() => (
              <PoisonCard
                q={q}
                onDismissed={(id) => {
                  setDismissed((prev) => new Set(prev).add(id));
                }}
              />
            )}
          >
            <QuestionCard
              q={q}
              session={byId.get(q.session_id)}
              now={now}
              selected={q.id === selQ}
              onSelect={() => {
                onSelect(q.id);
              }}
              onDismissed={(id) => {
                setDismissed((prev) => new Set(prev).add(id));
              }}
              onOpenTerm={onOpenTerm}
            />
          </ErrorBoundary>
        ))}
      </div>
    </div>
  );
}
