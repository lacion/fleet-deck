// plans.mjs — the v1.3 plan library mark endpoint (POST /api/plans/:id/mark)
// and the BUG-039 assign endpoint (POST /api/plans/:id/assign). Capture
// happens in events.mjs (hookHoldQuestion at ExitPlanMode); this module
// records the executed/archived verdict and composes daemon-authorized
// assignment mail. Threaded ctx state: q, tick, onMutate, mail,
// resolveTargets, and (BUG-041) ctx.questions.dismiss to retire a planner
// hold left pending when its plan is marked executed.

export function createPlans(ctx) {
  const { q, tick, onMutate, mail, resolveTargets } = ctx;

  // ------------------------------------------- v1.3 plan library (mark)
  // POST /api/plans/:id/mark {status, via?} — control API, real status codes.
  // Transition matrix (CONTRACT "B. Plan library" LIBRARY):
  //   → executed:  from proposed | approved | captured  (optional {via}
  //                string recorded on the row); rejected/executed/
  //                handled-in-terminal/archived → 409 bad transition.
  //   → archived:  from ANY non-archived status (proposed, approved,
  //                captured, rejected, handled-in-terminal, executed);
  //                archived → 409.
  // Any other target status → 400 (this endpoint only marks executed /
  // archived; the answer paths own approved/captured/rejected, and the
  // daemon's activity-settle path owns handled-in-terminal — derive.mjs
  // planRetired/settleTerminalPlans, UX 2.2). 404 unknown.
  // Execution/assignment rides the daemon-side assignPlan below (BUG-039);
  // this endpoint only records the verdict.
  const EXECUTABLE_FROM = new Set(['proposed', 'approved', 'captured']);
  function planMark(plan_id, body) {
    const p = q.getPlan.get(Number(plan_id));
    if (!p) return { status: 404, body: { ok: false, err: 'no such plan' } };
    const target = body?.status;
    if (target !== 'executed' && target !== 'archived') {
      return { status: 400, body: { ok: false, err: 'status must be "executed" or "archived"' } };
    }
    if (body?.via != null && typeof body.via !== 'string') {
      return { status: 400, body: { ok: false, err: 'via must be a string' } };
    }
    if (target === 'executed') {
      if (!EXECUTABLE_FROM.has(p.status)) {
        return { status: 409, body: { ok: false, err: `cannot mark a ${p.status} plan executed` } };
      }
      const via = body?.via?.trim() ? body.via.trim().slice(0, 200) : null;
      q.setPlanExecuted.run(via, p.plan_id);
      tick(`📚 plan #${p.plan_id} (${p.callsign ?? p.session_id}) marked executed${via ? ` via ${via}` : ''}`);
      // BUG-041 (daemon half): marking executed while the planner's own
      // ExitPlanMode question is STILL PENDING means the plan now lives on a
      // different worker — the planner must not sit parked on a stale prompt
      // for it. Retire that question through the ordinary dismiss path (fails
      // a live hold open with {} so the terminal chooser renders/owns the
      // decision). activity:true because the dismissal is itself the execution
      // decision: any FURTHER 'proposed' plan whose question was retired
      // earlier settles at the planner's next activity as usual, never here.
      if (p.question_id != null) {
        const qq = ctx.questions?.dismiss?.(p.question_id, { activity: true });
        if (qq?.ok && !qq.already) {
          tick(`📚 planner hold for plan #${p.plan_id} retired — question dismissed`);
        }
      }
    } else {
      if (p.status === 'archived') {
        return { status: 409, body: { ok: false, err: 'plan is already archived' } };
      }
      q.setPlanStatus.run('archived', p.plan_id);
      tick(`📚 plan #${p.plan_id} (${p.callsign ?? p.session_id}) archived`);
    }
    onMutate();
    return { status: 200, body: { ok: true, plan_id: p.plan_id, status: target } };
  }

  // ------------------------------------------ v1.3 plan library (assign)
  // POST /api/plans/:id/assign {to, instructions?} — BUG-039. The board's
  // Assign control used to COMPOSE the [FLEETDECK ASSIGNMENT] frame client-side
  // and post it through POST /mail — where postMail 422s every reserved frame
  // (mail.mjs RESERVED_FRAME_RE, 0.16.0), so Assign never queued a message and
  // the plan was never marked executed. Only the daemon's internal mail() may
  // wear that frame, so the assignment is composed HERE: validate the plan and
  // target, insert the authorized frame through mail(), then record the
  // assignment with the ordinary planMark('executed') path above (same
  // transition matrix, same BUG-041 planner-hold retirement — a 409 there
  // means the plan already settled and the target never gets the assignment,
  // because the mail is only sent after the claim succeeded).
  function assignPlan(plan_id, body) {
    const p = q.getPlan.get(Number(plan_id));
    if (!p) return { status: 404, body: { ok: false, err: 'no such plan' } };
    const to = typeof body?.to === 'string' ? body.to.trim() : '';
    if (!to) return { status: 400, body: { ok: false, err: 'to must be a session id or callsign' } };
    if (body?.instructions != null && typeof body.instructions !== 'string') {
      return { status: 400, body: { ok: false, err: 'instructions must be a string' } };
    }
    if (!EXECUTABLE_FROM.has(p.status)) {
      return { status: 409, body: { ok: false, err: `cannot assign a ${p.status} plan` } };
    }
    const target = resolveTargets(to).map(sid => q.getSession.get(sid))
      .find(s => s && s.ended_at == null);
    if (!target) return { status: 404, body: { ok: false, err: `no live session matching "${to}"` } };
    const marked = planMark(p.plan_id, { status: 'executed', via: `assign:${target.session_id}` });
    if (!marked.body.ok) return marked; // 409: plan already settled — no mail sent
    const instr = body?.instructions?.trim();
    const text = `[FLEETDECK ASSIGNMENT] Execute this approved plan exactly. Custom instructions: ${instr || ''}\n\n---\n${p.plan_md ?? ''}`;
    mail(target.session_id, 'orchestrator', text);
    tick(`📚 plan #${p.plan_id} assigned to ${target.callsign ?? target.session_id}`);
    onMutate();
    return { status: 200, body: { ok: true, plan_id: p.plan_id, status: 'executed', session_id: target.session_id, callsign: target.callsign ?? null } };
  }

  return { planMark, assignPlan };
}
