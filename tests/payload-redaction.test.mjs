// tests/payload-redaction.test.mjs — the "secrets are scrubbed" promise made
// executable. FLEETDECK_CAPTURE_PAYLOADS is a raw-telemetry escape hatch
// (README env table; SECURITY.md capture threat), so these assertions pin that
// the three redaction layers — secret KEY names, known credential SHAPES in
// string values, and the daemon's own token — actually fire on the bytes that
// reach hook-payloads.jsonl, and that a giant secret is MASKED rather than
// merely truncated-but-leaked.

import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createPayloadCapture, redactDiagnosticText, scrubUrlCredentials } from '../scripts/fleetd/payload-capture.mjs';

function scratchHome(t) {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-redact-'));
  t.after(() => rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  return dir;
}

// Capture one payload and hand back BOTH the parsed structure (for value
// assertions) and the raw file bytes (for leak assertions — a redacted field
// still proves nothing if the secret survives elsewhere on the line).
function captureOnce(t, payload, opts = {}) {
  const dir = scratchHome(t);
  const file = path.join(dir, 'hook-payloads.jsonl');
  createPayloadCapture(dir, { enabled: true, ...opts })('Stop', payload);
  const raw = readFileSync(file, 'utf8');
  return { raw, payload: JSON.parse(raw).payload };
}

test('secret-looking keys redact whole and are never descended into; siblings survive', (t) => {
  const { payload, raw } = captureOnce(t, {
    tool_input: {
      env: { GITHUB_TOKEN: 'ghp_' + 'a'.repeat(30), AWS_SECRET_ACCESS_KEY: 'y'.repeat(40) },
      headers: { Authorization: 'Bearer ' + 'z'.repeat(30) },
      apiKey: 'k'.repeat(20),        // camelCase — the [_\-.] boundary never fires,
      authToken: 'q'.repeat(20),     // so isSecretKey normalizes the hump to '_' first
      accessKeyId: 'AKIA' + 'B'.repeat(16),
      cwd: '/home/dev/project',
      model: 'claude-opus-4',
    },
  });
  const ti = payload.tool_input;
  assert.equal(ti.env.GITHUB_TOKEN, '[redacted]');
  assert.equal(ti.env.AWS_SECRET_ACCESS_KEY, '[redacted]');
  assert.equal(ti.headers.Authorization, '[redacted]');
  assert.equal(ti.apiKey, '[redacted]');
  assert.equal(ti.authToken, '[redacted]');
  assert.equal(ti.accessKeyId, '[redacted]');
  // Sibling non-secret keys keep their exact values.
  assert.equal(ti.cwd, '/home/dev/project');
  assert.equal(ti.model, 'claude-opus-4');
  // And not one raw secret byte reached disk (the value was never walked).
  for (const leak of ['ghp_', 'AKIA', 'zzzzz', 'yyyyy', 'kkkkk', 'qqqqq']) {
    assert.equal(raw.includes(leak), false, `${leak} must not appear on disk`);
  }
});

test('innocent keys that merely contain a secret substring survive verbatim', (t) => {
  const { payload } = captureOnce(t, {
    tokenizer: 'gpt2',
    authored: 'jane@example.com',
    monotonic: '12345',
  });
  assert.equal(payload.tokenizer, 'gpt2');
  assert.equal(payload.authored, 'jane@example.com');
  assert.equal(payload.monotonic, '12345');
});

test('known credential shapes are masked mid-string; surrounding text is kept', (t) => {
  const pem = '-----BEGIN RSA PRIVATE KEY-----\nMIIEabc123def456\n-----END RSA PRIVATE KEY-----';
  // A block the byte budget could have cut off before its END marker.
  const truncatedPem = '-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAAdeadbeef';
  const jwt = 'eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJVadQssw5c';
  const { payload } = captureOnce(t, {
    log: `anthropic key sk-ant-api03-${'A'.repeat(40)} used here`,
    gh: `pushed with ghp_${'b'.repeat(36)} token`,
    slack: `hook xoxb-${'1'.repeat(24)}-abc posted`,
    aws: `role AKIA${'C'.repeat(16)} assumed`,
    session: `jwt=${jwt};`,
    key: `begin ${pem} end`,          // key name 'key' does NOT match — tests VALUE path
    truncated: `pem: ${truncatedPem}`,
    hdr: `Authorization: Bearer ${'d'.repeat(40)} sent`,
  });
  assert.equal(payload.log, 'anthropic key [redacted] used here');
  assert.equal(payload.gh, 'pushed with [redacted] token');
  assert.equal(payload.slack, 'hook [redacted] posted');
  assert.equal(payload.aws, 'role [redacted] assumed');
  assert.equal(payload.session, 'jwt=[redacted];');
  assert.equal(payload.key, 'begin [redacted] end');
  assert.equal(payload.truncated, 'pem: [redacted]');
  assert.equal(payload.hdr, 'Authorization: [redacted] sent');
});

test('a known daemon secret is scrubbed from the finished line, bytes and all', (t) => {
  const secret = 'a1b2c3d4e5f60718293a4b5c6d7e8f90'; // 32-hex fixture, no telltale shape
  const { payload, raw } = captureOnce(t, {
    prompt: `here is my token ${secret} embedded in prose`,
    note: 'unrelated',
  }, { secrets: [secret] });
  assert.equal(raw.includes(secret), false, 'the raw token bytes must be absent from the file');
  assert.equal(payload.prompt, 'here is my token [redacted] embedded in prose');
  assert.equal(payload.note, 'unrelated');
});

test('an adversarial JWT-shaped string cannot stall the synchronous capture (ReDoS guard)', (t) => {
  // Pre-fix, the JWT shape /eyJ…{10,}\.{10,}\.{10,}/ backtracked quadratically:
  // on ('eyJ'.repeat(N) + '.' + 'a'.repeat(M)) the first unbounded run rescans
  // to the lone dot at every 'eyJ' start (~4.5s at this size, measured). With
  // each segment bounded to {10,4096} per-start work is constant → linear.
  // Capture runs synchronously inside the hook handler, so this MUST stay fast.
  const evil = 'eyJ'.repeat(32000) + '.' + 'a'.repeat(1000); // ~97 KB
  const started = Date.now();
  const { raw } = captureOnce(t, { blob: evil }, { maxPayloadBytes: 97_000 });
  const elapsed = Date.now() - started;
  // The string is not a real JWT (only one dot), so it is not masked — the
  // point is purely that redaction returns promptly and a record is written.
  assert.ok(raw.length > 0, 'capture produced a line');
  assert.ok(elapsed < 2_000, `redaction must not hang; took ${elapsed}ms`);
});

test('an operator token with JSON-special chars is scrubbed in every form, bytes and all', (t) => {
  // Generated tokens are hex (safe), but an operator-set FLEETDECK_TOKEN may
  // contain " \ or control chars. Inside the JSON line those appear only in
  // escaped form, so the raw split() misses them — the escaped-inner scrub is
  // what closes the leak.
  const secret = 'ab"cd\\ef012345678'; // 17 chars: contains a quote and a backslash
  const escapedInner = JSON.stringify(secret).slice(1, -1); // how it appears in the line
  const { payload, raw } = captureOnce(t, {
    prompt: `operator pasted ${secret} into the box`,
    note: 'unrelated',
  }, { secrets: [secret] });
  assert.equal(raw.includes(secret), false, 'raw token bytes must be absent');
  assert.equal(raw.includes(escapedInner), false, 'JSON-escaped token bytes must be absent too');
  assert.equal(raw.includes('012345678'), false, 'no distinctive tail of the token may survive');
  assert.equal(payload.prompt, 'operator pasted [redacted] into the box');
  assert.equal(payload.note, 'unrelated');
});

test('scrubUrlCredentials removes URL userinfo, bytes and all, and is idempotent', () => {
  const secret = 'glpat-AbCdEf1234567890';
  const line = `fatal: unable to access 'https://luis:${secret}@gitlab.com/x/y.git/'`;
  const out = scrubUrlCredentials(line);
  assert.equal(out, "fatal: unable to access 'https://[redacted]@gitlab.com/x/y.git/'");
  assert.equal(out.includes(secret), false, 'the raw token bytes must be gone');
  assert.equal(out.includes('luis'), false, 'the username half goes too — a bare token is indistinguishable from one');
  assert.equal(scrubUrlCredentials(out), out, 'a second pass must be a no-op');
  // Shapes it must NOT touch: no userinfo, and scp-style (which has no password
  // slot and whose legibility is the point of the git-stderr expander).
  assert.equal(scrubUrlCredentials('https://github.com/settings/ssh/new'), 'https://github.com/settings/ssh/new');
  assert.equal(scrubUrlCredentials('git@github.com:owner/repo.git'), 'git@github.com:owner/repo.git');
});

test('redactDiagnosticText is UNCHANGED: it still has no userinfo rule', () => {
  // Pinned deliberately. scrubUrlCredentials is a SEPARATE export precisely so
  // hook-payload capture stays bit-for-bit identical; if a future reader folds a
  // userinfo pattern into SECRET_VALUE_RES instead, this fails and points at the
  // capture-format cases above that would then need revisiting. The corollary
  // for callers: the shape scrubber alone is NOT sufficient for a credentialed
  // URL — they must compose both, as gitStderrDetail does.
  const line = "fatal: unable to access 'https://luis:glpat-AbCdEf1234567890@gitlab.com/x/y.git/'";
  assert.equal(redactDiagnosticText(line), line);
});

test('a giant value under a secret key is redacted, never truncated-but-leaked', (t) => {
  const giant = 'S3CR3T' + 'x'.repeat(400_000);
  const { payload, raw } = captureOnce(t, { password: giant, ok: 'visible' });
  assert.equal(payload.password, '[redacted]');
  assert.equal(raw.includes('S3CR3T'), false, 'no prefix of the secret may leak');
  assert.equal(raw.includes('xxxxxxxxxx'), false, 'not even a bounded slice of it leaks');
  assert.ok(Buffer.byteLength(raw) < 2_000, 'the giant value never materialized into the line');
  assert.equal(payload.ok, 'visible');
});
