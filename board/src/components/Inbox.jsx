import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { human, basename, questionView, sessionsById, TURN_BOUNDARY_HINT } from '../util.js';
import { renderMarkdown, planTitle } from '../markdown.js';
import { answerQuestion, dismissQuestion, reasonOf } from '../api.js';
import { registerQuestion } from '../qbus.js';

// The fixed right rail: NEEDS YOU. Global (never repo-filtered — the human's
// attention is the one resource that isn't per-project), badged by repo.

function CountdownRing({ q, now }) {
  if (!q.expires_at) return null;
  const total = Math.max(1, q.expires_at - (q.created_at || q.expires_at - 50000));
  const secs = Math.max(0, Math.ceil((q.expires_at - now) / 1000));
  const frac = Math.max(0, Math.min(1, (q.expires_at - now) / total));
  const color = secs <= 12 ? 'var(--hazard)' : 'var(--act)';
  return (
    <span className="fd-ring" title={`${secs}s until this falls back to the terminal`}>
      <svg width="26" height="26" viewBox="0 0 26 26">
        <circle cx="13" cy="13" r="10.5" fill="none" stroke="var(--border)" strokeWidth="2.5" />
        <circle
          className="arc"
          cx="13" cy="13" r="10.5" fill="none"
          stroke={color} strokeWidth="2.5" strokeLinecap="round"
          strokeDasharray="66" strokeDashoffset={66 * (1 - frac)}
        />
      </svg>
      <span className="secs" style={{ color }}>{secs}</span>
    </span>
  );
}

function PermissionBody({ view, busy, onAnswer }) {
  return (
    <>
      {view.command && !view.diff && <div className="cmd">{view.command}</div>}
      {view.diff && (
        <div className="cmd">
          <span style={{ color: 'var(--dim)' }}>{view.command}{'\n'}</span>
          {view.diff.map((l, i) => (
            <span key={i} className={l.kind}>{l.text}{'\n'}</span>
          ))}
        </div>
      )}
      <div className="fd-btnrow">
        <button type="button" className="fd-allow" disabled={busy} onClick={() => onAnswer({ behavior: 'allow' }, 'allow')}>
          Allow <span className="k">y</span>
        </button>
        <button type="button" className="fd-deny" disabled={busy} onClick={() => onAnswer({ behavior: 'deny' }, 'deny')}>
          Deny <span className="k">n</span>
        </button>
      </div>
    </>
  );
}

// v1.3 — an ExitPlanMode permission renders as a PLAN card: the rendered
// plan (tiny local markdown, everything escaped), then Approve / Capture &
// release / Deny. Approve/Deny keep the .fd-allow/.fd-deny classes so the
// y/n keys keep working; capture is the board-only pseudo-behavior (the
// daemon denies the hook AND mails the planner to stop without executing).
function PlanBody({ q, busy, onAnswer }) {
  const md = q.payload?.tool_input?.plan || '';
  // M-P7 — parse the plan markdown once per body text, not on every 1 s render.
  // M-P2 — a resolved snapshot may omit the plan body; render a placeholder
  // rather than an empty box (never assume the body is present).
  const html = useMemo(() => renderMarkdown(md), [md]);
  return (
    <>
      {md.trim()
        ? <div className="fd-md rail" dangerouslySetInnerHTML={{ __html: html }} />
        : <div className="fd-md rail"><em>plan text not included in this snapshot</em></div>}
      <div className="fd-btnrow">
        <button type="button" className="fd-allow" disabled={busy} onClick={() => onAnswer({ behavior: 'allow' }, 'approve')}>
          Approve <span className="k">y</span>
        </button>
        <button type="button" className="fd-deny" disabled={busy} onClick={() => onAnswer({ behavior: 'deny' }, 'deny')}>
          Deny <span className="k">n</span>
        </button>
      </div>
      <button
        type="button"
        className="fd-capture"
        disabled={busy}
        title="store the plan; the planner stops without executing"
        onClick={() => onAnswer({ behavior: 'capture' }, 'capture & release')}
      >
        Capture &amp; release
      </button>
    </>
  );
}

function ChoiceBody({ view, busy, onAnswer, bindKeys }) {
  const questions = view.questions || [];
  const [picked, setPicked] = useState({}); // question text -> label | [labels]
  const multi = questions.length > 1 || questions.some((x) => x.multiSelect);

  const pick = (question, opt) => {
    if (!multi) {
      onAnswer({ answers: { [question.question]: opt.label } }, opt.label);
      return;
    }
    setPicked((prev) => {
      const cur = prev[question.question];
      if (question.multiSelect) {
        const arr = Array.isArray(cur) ? [...cur] : [];
        const i = arr.indexOf(opt.label);
        if (i === -1) arr.push(opt.label); else arr.splice(i, 1);
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
          const opts = question.options || [];
          if (idx < opts.length) { pick(question, opts[idx]); return; }
          idx -= opts.length;
        }
      },
    });
    return () => bindKeys?.(null);
    // pick closes over `picked`/`multi`; re-bind when those (or busy) change
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [view, picked, busy, bindKeys]);

  const complete = questions.every((x) => {
    const v = picked[x.question];
    return Array.isArray(v) ? v.length > 0 : !!v;
  });
  const submit = () => {
    const answers = {};
    for (const x of questions) answers[x.question] = picked[x.question];
    onAnswer({ answers }, 'answers sent');
  };

  return (
    <>
      {questions.map((question, qi) => (
        <div key={qi} style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
          {question.header && <span className="fd-choicehead">{question.header.toUpperCase()}</span>}
          {qi > 0 && <div className="title" style={{ fontWeight: 500 }}>{question.question}</div>}
          <div className="fd-opts">
            {(question.options || []).map((opt, i) => {
              const cur = picked[question.question];
              const isPicked = Array.isArray(cur) ? cur.includes(opt.label) : cur === opt.label;
              return (
                <button
                  key={i}
                  type="button"
                  className={`fd-opt${isPicked ? ' picked' : ''}`}
                  disabled={busy}
                  onClick={() => pick(question, opt)}
                >
                  <span className="n">{i + 1}</span>
                  <span className="body">
                    <span className="l">{opt.label}</span>
                    {opt.description && <span className="d">{opt.description}</span>}
                  </span>
                </button>
              );
            })}
          </div>
          {question.multiSelect && <span className="micro">multi-select — pick any, then send</span>}
        </div>
      ))}
      {multi && (
        <div className="fd-freerow">
          <span className="fd-spacer" />
          <button type="button" className="fd-send" disabled={busy || !complete} onClick={submit}>Send</button>
        </div>
      )}
    </>
  );
}

function FreeformBody({ offline, busy, onAnswer, bindKeys }) {
  const [draft, setDraft] = useState('');
  const taRef = useRef(null);
  const send = () => { if (draft.trim()) onAnswer({ text: draft.trim() }, 'sent'); };
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
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); send(); }
          }}
        />
        <button type="button" className="fd-send" disabled={busy || !draft.trim()} onClick={send}>Send</button>
      </div>
      <span className="micro">
        {offline
          ? 'session is offline — your answer delivers on resume'
          : `delivered at ${TURN_BOUNDARY_HINT}`}
      </span>
    </>
  );
}

// Schema-driven basic form for elicitation, with a raw-JSON fallback.
function ElicitationBody({ view, busy, onAnswer }) {
  const props = view.schema?.properties && typeof view.schema.properties === 'object'
    ? Object.entries(view.schema.properties)
    : [];
  const simple = props.length > 0 && props.every(([, def]) =>
    def && (def.enum || ['string', 'number', 'integer', 'boolean'].includes(def.type)));
  const [useJson, setUseJson] = useState(!simple);
  const [fields, setFields] = useState({});
  const [jsonDraft, setJsonDraft] = useState('{\n}');
  const [jsonErr, setJsonErr] = useState(null);
  const required = new Set(view.schema?.required || []);

  const accept = () => {
    if (useJson) {
      try {
        const content = JSON.parse(jsonDraft);
        onAnswer({ action: 'accept', content }, 'accepted');
      } catch {
        setJsonErr('not valid JSON');
      }
      return;
    }
    const content = {};
    for (const [name, def] of props) {
      const v = fields[name];
      if (v === undefined || v === '') continue;
      content[name] = def.type === 'number' || def.type === 'integer' ? Number(v)
        : def.type === 'boolean' ? !!v
        : v;
    }
    onAnswer({ action: 'accept', content }, 'accepted');
  };
  const missingRequired = !useJson && [...required].some((r) => {
    const v = fields[r];
    return v === undefined || v === '';
  });

  return (
    <>
      {!useJson ? (
        <div className="fd-form">
          {props.map(([name, def]) => (
            <div className="frow" key={name}>
              <span className="fl" title={def.description || name}>{name}{required.has(name) ? ' *' : ''}</span>
              {def.enum ? (
                <select
                  className="fd-input"
                  value={fields[name] ?? ''}
                  onChange={(e) => setFields({ ...fields, [name]: e.target.value })}
                >
                  <option value="" disabled>choose…</option>
                  {def.enum.map((v) => <option key={String(v)} value={v}>{String(v)}</option>)}
                </select>
              ) : def.type === 'boolean' ? (
                <input
                  type="checkbox"
                  checked={!!fields[name]}
                  onChange={(e) => setFields({ ...fields, [name]: e.target.checked })}
                />
              ) : (
                <input
                  className="fd-input"
                  type={def.type === 'number' || def.type === 'integer' ? 'number' : 'text'}
                  placeholder={def.description || def.type || ''}
                  value={fields[name] ?? ''}
                  onChange={(e) => setFields({ ...fields, [name]: e.target.value })}
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
            onChange={(e) => { setJsonDraft(e.target.value); setJsonErr(null); }}
            spellCheck={false}
          />
          {jsonErr && <span className="status hazard">{jsonErr}</span>}
        </>
      )}
      <div className="fd-btnrow">
        <button type="button" className="fd-allow" disabled={busy || missingRequired} onClick={accept}>Accept</button>
        <button type="button" className="fd-deny" disabled={busy} onClick={() => onAnswer({ action: 'decline' }, 'declined')}>
          Decline
        </button>
      </div>
      {simple && (
        <button
          type="button"
          className="micro"
          style={{ background: 'none', border: 'none', color: 'var(--faint)', cursor: 'pointer', textAlign: 'left', padding: 0 }}
          onClick={() => setUseJson(!useJson)}
        >
          {useJson ? '← back to the form' : 'answer as raw JSON instead'}
        </button>
      )}
    </>
  );
}

const KIND_LABEL = { permission: 'PERMISSION', choice: 'CHOICE', freeform: 'FREE-TEXT', elicitation: 'FORM' };

function statusLine(q, session) {
  if (q.status === 'answered') {
    if (q.kind === 'freeform') {
      const offline = session?.col === 'offline';
      return { cls: 'ok', text: offline ? '✓ queued — delivers on resume' : `✓ queued — delivers at ${TURN_BOUNDARY_HINT}` };
    }
    return { cls: 'ok', text: '✓ answered from the board' };
  }
  if (q.status === 'expired') return { cls: '', text: '⏱ expired — the terminal owns this one' };
  return null;
}

function QuestionCard({ q, session, now, selected, onSelect, onDismissed }) {
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState(null); // transient result of an answer POST
  // v1.8 — dismiss: the question you already answered in the terminal. Low
  // risk (the daemon expires it and sends the session NOTHING), so no confirm
  // — just an in-flight lock and an honest failure line if the daemon says no.
  const [dismissing, setDismissing] = useState(false);
  const view = questionView(q);
  // v1.3 — ExitPlanMode permissions are PLAN cards (they also carry plan_id)
  const isPlan = q.kind === 'permission' && q.payload?.tool_name === 'ExitPlanMode';
  const pending = q.status === 'pending';
  const holdKind = q.kind !== 'freeform';
  const holdLost = pending && holdKind && q.held === false;
  const done = !pending;
  const repoName = session?.repo_name || (session?.repo_id ? basename(session.repo_id) : null);
  const offline = session?.col === 'offline';
  // M-P7 — the plan title is derived from the plan body; memo it so a 1 s
  // re-render doesn't re-scan the markdown. M-P2 — planTitle already degrades
  // to 'untitled plan' when the body is absent.
  const planMd = isPlan ? (q.payload?.tool_input?.plan || '') : '';
  const title = useMemo(
    () => (isPlan ? planTitle(planMd) : view.title),
    [isPlan, planMd, view.title],
  );

  const onAnswer = useCallback(async (body, label) => {
    setBusy(true);
    setNote(null);
    const res = await answerQuestion(q.id, body);
    if (res.ok) {
      setNote({
        cls: q.kind === 'freeform' ? 'act' : 'ok',
        text: q.kind === 'freeform'
          ? (offline ? `→ ${label} · queued — delivers on resume` : `→ ${label} · queued — delivers at ${TURN_BOUNDARY_HINT}`)
          : `→ ${label} — sent to agent`,
      });
    } else {
      setNote({ cls: 'hazard', text: reasonOf(res, `answer failed (${res.status})`) });
    }
    setBusy(false);
  }, [q.id, q.kind, offline]);

  const doDismiss = async () => {
    if (dismissing) return;
    setDismissing(true);
    setNote(null);
    const res = await dismissQuestion(q.id);
    if (res.ok && res.json?.ok !== false) {
      // gone from the rail immediately; the next snapshot agrees (expired)
      onDismissed?.(q.id);
      return; // unmounting — don't touch state
    }
    setNote({ cls: 'hazard', text: reasonOf(res, `dismiss failed (${res.status})`) });
    setDismissing(false);
  };

  const resolved = statusLine(q, session);
  const interactive = pending && !holdLost;

  // M-F6 — the imperative handle App's hotkeys reach this card by. Registered
  // per id; the body components fill in `bodyApi` (choose / focusInput) so no
  // CSS class is load-bearing for the keyboard path.
  const cardRef = useRef(null);
  const bodyApi = useRef({});
  const bindKeys = useCallback((api) => { bodyApi.current = api || {}; }, []);
  useEffect(() => {
    const handle = {
      allow: () => { if (interactive && !busy) onAnswer({ behavior: 'allow' }, isPlan ? 'approve' : 'allow'); },
      deny: () => { if (interactive && !busy) onAnswer({ behavior: 'deny' }, 'deny'); },
      choose: (n) => { if (interactive && !busy) bodyApi.current.choose?.(n); },
      focusInput: () => { bodyApi.current.focusInput?.(); },
      scrollIntoView: () => { cardRef.current?.scrollIntoView({ block: 'nearest' }); },
    };
    return registerQuestion(q.id, handle);
  }, [q.id, interactive, busy, isPlan, onAnswer]);

  return (
    <div
      ref={cardRef}
      className={`fd-q${selected && pending ? ' sel' : ''}${done ? ' done' : ''}`}
      onClick={() => { if (pending) onSelect(); }}
    >
      <div className="row1">
        <span className="callsign">{q.callsign || q.session_id}</span>
        <span className={`fd-kind ${isPlan ? 'plan' : q.kind}`}>
          {isPlan ? 'PLAN' : KIND_LABEL[q.kind] || q.kind?.toUpperCase()}
        </span>
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
            onClick={(e) => { e.stopPropagation(); doDismiss(); }}
          >
            ✕
          </button>
        )}
      </div>
      <div className="title">{title}</div>
      {holdLost && (
        <div className="status hazard">⚠ hold lost (daemon restarted) — decide in the terminal</div>
      )}
      {interactive && isPlan && <PlanBody q={q} busy={busy} onAnswer={onAnswer} />}
      {interactive && q.kind === 'permission' && !isPlan && <PermissionBody view={view} busy={busy} onAnswer={onAnswer} />}
      {interactive && q.kind === 'choice' && <ChoiceBody view={view} busy={busy} onAnswer={onAnswer} bindKeys={bindKeys} />}
      {interactive && q.kind === 'freeform' && <FreeformBody offline={offline} busy={busy} onAnswer={onAnswer} bindKeys={bindKeys} />}
      {interactive && q.kind === 'elicitation' && <ElicitationBody view={view} busy={busy} onAnswer={onAnswer} />}
      {note && pending && <div className={`status ${note.cls}`}>{note.text}</div>}
      {resolved && <div className={`status ${resolved.cls}`}>{resolved.text}</div>}
    </div>
  );
}

export default function Inbox({ questions, sessions, now, selQ, onSelect }) {
  const byId = sessionsById(sessions);
  // v1.8 — dismissed ids stay hidden in this tab. The daemon expires the
  // question, but an expired question still rides the snapshot for a while
  // (F3: pending + the last few resolved) and would come back as a faded
  // "⏱ expired" card — which is exactly the stale clutter being cleared.
  const [dismissed, setDismissed] = useState(() => new Set());
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
          <div className="fd-allclear">
            <div className="ring">✓</div>
            <div className="t1">ALL CLEAR</div>
            <div className="t2">No one is waiting on you.</div>
          </div>
        )}
        {ordered.map((q) => (
          <QuestionCard
            key={q.id}
            q={q}
            session={byId.get(q.session_id)}
            now={now}
            selected={q.id === selQ}
            onSelect={() => onSelect(q.id)}
            onDismissed={(id) => setDismissed((prev) => new Set(prev).add(id))}
          />
        ))}
      </div>
    </div>
  );
}
