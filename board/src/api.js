// Thin wrappers over the fleetd board/control API.

async function post(url, body) {
  const res = await fetch(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body ?? {}),
  });
  let json = null;
  try { json = await res.json(); } catch { /* non-JSON error body */ }
  return { status: res.status, ok: res.ok, json };
}

export function answerQuestion(id, body) {
  return post(`/api/questions/${id}/answer`, body);
}

// to = session_id | callsign | 'all' | 'repo:<name>'
export function sendMail(to, text) {
  return post('/mail', { to, from: 'board', text });
}

export function sendCommand(text) {
  return post('/command', { text });
}

// v1.2 — board-spawned sessions. Spawn is an explicit human click ONLY.
export function spawnSession(body) {
  return post('/api/spawn', body);
}

export function killSpawn(spawnId, force) {
  return post(`/api/spawn/${encodeURIComponent(spawnId)}/kill`, { force: !!force });
}

// v1.3 — plan library. body = {status:'executed', via} | {status:'archived'};
// the daemon 409s bad transitions and the board surfaces that honestly.
export function markPlan(planId, body) {
  return post(`/api/plans/${encodeURIComponent(planId)}/mark`, body);
}
