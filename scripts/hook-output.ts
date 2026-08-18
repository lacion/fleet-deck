// Runtime firewall for daemon -> Claude hook output. The daemon is local, but
// its port can be occupied by another process and mixed-version rollouts are
// real. Only the small response dialect Fleet Deck intentionally uses crosses
// into a Claude session; everything else canonicalizes to the invisible `{}`.

// Claude Code's documented command-hook output cap.
const MAX_CONTEXT_CHARS = 10_000;
const MAX_STRUCTURED_JSON_BYTES = 256 * 1024;
const MAX_JSON_DEPTH = 12;
const MAX_JSON_NODES = 4096;
const FORBIDDEN_KEYS = new Set(['__proto__', 'constructor', 'prototype']);

type JsonRecord = Record<string, unknown>;

function record(value: unknown): JsonRecord | null {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as JsonRecord)
    : null;
}

function exactKeys(value: JsonRecord, required: string[], optional: string[] = []): boolean {
  const keys = Object.keys(value);
  const allowed = new Set([...required, ...optional]);
  return (
    required.every((key) => Object.hasOwn(value, key)) && keys.every((key) => allowed.has(key))
  );
}

function safeJson(value: unknown): boolean {
  let nodes = 0;
  const visit = (candidate: unknown, depth: number): boolean => {
    nodes += 1;
    if (nodes > MAX_JSON_NODES || depth > MAX_JSON_DEPTH) return false;
    if (candidate === null || typeof candidate === 'boolean' || typeof candidate === 'string') {
      return true;
    }
    if (typeof candidate === 'number') return Number.isFinite(candidate);
    if (Array.isArray(candidate)) return candidate.every((item) => visit(item, depth + 1));
    const obj = record(candidate);
    if (!obj) return false;
    return Object.entries(obj).every(
      ([key, nested]) => !FORBIDDEN_KEYS.has(key) && visit(nested, depth + 1),
    );
  };
  if (!visit(value, 0)) return false;
  try {
    return Buffer.byteLength(JSON.stringify(value)) <= MAX_STRUCTURED_JSON_BYTES;
  } catch {
    return false;
  }
}

/** Trust boundary for the only intentional SessionStart model context. */
export function trustedRosterBrief(registration: unknown): string | null {
  const reg = record(registration);
  if (!reg) return null;
  const keys = Object.keys(reg).sort();
  const normalKeys =
    keys.length === 3 && keys[0] === 'brief' && keys[1] === 'callsign' && keys[2] === 'ok';
  const takeoverKeys =
    keys.length === 4 &&
    keys[0] === 'brief' &&
    keys[1] === 'callsign' &&
    keys[2] === 'ok' &&
    keys[3] === 'upgrade_lines';
  if (!normalKeys && !takeoverKeys) return null;

  // A managed-daemon handoff may include diagnostics for CLI consumers. The
  // hook validates but deliberately never prints them into Claude context.
  const upgradeLines = reg['upgrade_lines'];
  if (
    takeoverKeys &&
    (!Array.isArray(upgradeLines) ||
      upgradeLines.length > 16 ||
      upgradeLines.some(
        (line) => typeof line !== 'string' || line.length === 0 || line.length > 1_000,
      ))
  ) {
    return null;
  }
  const callsign = reg['callsign'];
  const brief = reg['brief'];
  if (reg['ok'] !== true || typeof callsign !== 'string' || !callsign || callsign.length > 256) {
    return null;
  }
  if (typeof brief !== 'string' || !brief || brief.length > MAX_CONTEXT_CHARS) return null;
  const prefix = `[FLEETDECK] You are on the fleet board as "${callsign}"`;
  return brief.startsWith(prefix) ? brief : null;
}

function contextResponse(event: string, parsed: JsonRecord): JsonRecord | null {
  if (!exactKeys(parsed, ['hookSpecificOutput'])) return null;
  const output = record(parsed['hookSpecificOutput']);
  if (!output || !exactKeys(output, ['hookEventName', 'additionalContext'])) return null;
  if (output['hookEventName'] !== event) return null;
  const context = output['additionalContext'];
  if (typeof context !== 'string' || !context || context.length > MAX_CONTEXT_CHARS) return null;
  return { hookSpecificOutput: { hookEventName: event, additionalContext: context } };
}

function stopResponse(parsed: JsonRecord): JsonRecord | null {
  if (!exactKeys(parsed, ['decision', 'reason'])) return null;
  const reason = parsed['reason'];
  if (parsed['decision'] !== 'block' || typeof reason !== 'string' || !reason) return null;
  if (reason.length > MAX_CONTEXT_CHARS) return null;
  return { decision: 'block', reason };
}

function permissionResponse(parsed: JsonRecord): JsonRecord | null {
  if (!exactKeys(parsed, ['hookSpecificOutput'])) return null;
  const output = record(parsed['hookSpecificOutput']);
  if (!output || !exactKeys(output, ['hookEventName', 'decision'])) return null;
  if (output['hookEventName'] !== 'PermissionRequest') return null;
  const decision = record(output['decision']);
  if (!decision || !exactKeys(decision, ['behavior'])) return null;
  const behavior = decision['behavior'];
  if (behavior !== 'allow' && behavior !== 'deny') return null;
  return {
    hookSpecificOutput: { hookEventName: 'PermissionRequest', decision: { behavior } },
  };
}

function askUserQuestionResponse(parsed: JsonRecord, requestRaw: string): JsonRecord | null {
  if (!exactKeys(parsed, ['hookSpecificOutput'])) return null;
  const output = record(parsed['hookSpecificOutput']);
  if (
    !output ||
    !exactKeys(output, ['hookEventName', 'permissionDecision', 'updatedInput']) ||
    output['hookEventName'] !== 'PreToolUse' ||
    output['permissionDecision'] !== 'allow'
  )
    return null;
  const updated = record(output['updatedInput']);
  if (!updated || !exactKeys(updated, ['questions', 'answers'])) return null;
  const questions = updated['questions'];
  const answers = record(updated['answers']);
  if (!Array.isArray(questions) || questions.length === 0 || !answers) return null;
  let expectedQuestions: unknown = null;
  try {
    expectedQuestions = record(record(JSON.parse(requestRaw))?.['tool_input'])?.['questions'];
  } catch {
    return null;
  }
  // A valid answer may enrich only the request that caused this exact hook.
  // Self-consistent but stale/foreign questions are still poison.
  if (
    !Array.isArray(expectedQuestions) ||
    JSON.stringify(questions) !== JSON.stringify(expectedQuestions)
  )
    return null;
  const questionTexts: string[] = [];
  for (const candidate of questions) {
    const question = record(candidate)?.['question'];
    if (typeof question !== 'string' || !question || questionTexts.includes(question)) return null;
    questionTexts.push(question);
  }
  const answerEntries = Object.entries(answers);
  if (
    answerEntries.length !== questionTexts.length ||
    answerEntries.some(
      ([question, answer]) =>
        !questionTexts.includes(question) ||
        typeof answer !== 'string' ||
        !answer ||
        answer.length > 2000,
    ) ||
    !safeJson({ questions, answers })
  )
    return null;
  return {
    hookSpecificOutput: {
      hookEventName: 'PreToolUse',
      permissionDecision: 'allow',
      updatedInput: { questions, answers },
    },
  };
}

function elicitationResponse(parsed: JsonRecord): JsonRecord | null {
  if (!exactKeys(parsed, ['hookSpecificOutput'])) return null;
  const output = record(parsed['hookSpecificOutput']);
  if (output?.['hookEventName'] !== 'Elicitation') return null;
  const action = output['action'];
  if (action !== 'accept' && action !== 'decline' && action !== 'cancel') return null;
  if (action === 'accept') {
    if (!exactKeys(output, ['hookEventName', 'action', 'content'])) return null;
    const content = record(output['content']);
    if (!content || !safeJson(content)) return null;
    return { hookSpecificOutput: { hookEventName: 'Elicitation', action, content } };
  }
  if (!exactKeys(output, ['hookEventName', 'action'], ['content'])) return null;
  const content = output['content'];
  if (
    content !== undefined &&
    (record(content) === null || Object.keys(content as JsonRecord).length > 0)
  ) {
    return null;
  }
  return { hookSpecificOutput: { hookEventName: 'Elicitation', action } };
}

/** Validate the response for this exact hook event and canonically reserialize it. */
export function canonicalHookOutput(
  event: string | undefined,
  text: string,
  requestRaw = '{}',
): string {
  if (!text || Buffer.byteLength(text) > 1024 * 1024) return '{}';
  let parsed: JsonRecord | null;
  try {
    parsed = record(JSON.parse(text));
  } catch {
    return '{}';
  }
  if (!parsed) return '{}';
  if (Object.keys(parsed).length === 0) return '{}';

  let allowed: JsonRecord | null = null;
  if (event === 'UserPromptSubmit' || event === 'PostToolUse' || event === 'PostToolUseFailure') {
    allowed = contextResponse(event, parsed);
  } else if (event === 'Stop') allowed = stopResponse(parsed);
  else if (event === 'PermissionRequest') allowed = permissionResponse(parsed);
  else if (event === 'AskUserQuestion') allowed = askUserQuestionResponse(parsed, requestRaw);
  else if (event === 'Elicitation') allowed = elicitationResponse(parsed);
  // CwdChanged, lifecycle/notification events, unknown future events, and every
  // mismatched response are deliberately invisible to Claude.
  return allowed ? JSON.stringify(allowed) : '{}';
}
