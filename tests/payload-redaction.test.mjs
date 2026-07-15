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
import { createPayloadCapture } from '../scripts/fleetd/payload-capture.mjs';

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

test('a giant value under a secret key is redacted, never truncated-but-leaked', (t) => {
  const giant = 'S3CR3T' + 'x'.repeat(400_000);
  const { payload, raw } = captureOnce(t, { password: giant, ok: 'visible' });
  assert.equal(payload.password, '[redacted]');
  assert.equal(raw.includes('S3CR3T'), false, 'no prefix of the secret may leak');
  assert.equal(raw.includes('xxxxxxxxxx'), false, 'not even a bounded slice of it leaks');
  assert.ok(Buffer.byteLength(raw) < 2_000, 'the giant value never materialized into the line');
  assert.equal(payload.ok, 'visible');
});
