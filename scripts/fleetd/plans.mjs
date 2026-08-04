// plans.mjs — the v1.3 plan library mark endpoint (POST /api/plans/:id/mark).
// Capture happens in events.mjs (hookHoldQuestion at ExitPlanMode); this module
// only records the executed/archived verdict. Threaded ctx state: q, tick,
// onMutate, and (BUG-041) ctx.questions.dismiss to retire a planner hold left
// pending when its plan is marked executed.

export function createPlans(ctx) {
  const { q, tick, onMutate } = ctx;

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
  // Execution/assignment is client-composed on existing machinery — this
  // endpoint only records the verdict.
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

  return { planMark };
}
