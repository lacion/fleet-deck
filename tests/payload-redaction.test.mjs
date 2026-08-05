// tests/payload-redaction.test.mjs — the "secrets are scrubbed" promise made
// executable. FLEETDECK_CAPTURE_PAYLOADS is a raw-telemetry escape hatch
// (README env table; SECURITY.md capture threat), so these assertions pin that
// the four redaction layers — secret KEY names, known credential SHAPES in
// string values, credentialed URLs (userinfo + secret query params), and the
// daemon's own token — actually fire on the bytes that reach
// hook-payloads.jsonl, and that a giant secret is MASKED rather than merely
// truncated-but-leaked.

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
      // PLURALS — a singular-only word list used to record these verbatim.
      api_keys: { primary: 'p'.repeat(20) },
      clientSecrets: ['c'.repeat(20)],
      access_tokens: ['t'.repeat(20)],
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
  assert.equal(ti.api_keys, '[redacted]');
  assert.equal(ti.clientSecrets, '[redacted]');
  assert.equal(ti.access_tokens, '[redacted]');
  // Sibling non-secret keys keep their exact values.
  assert.equal(ti.cwd, '/home/dev/project');
  assert.equal(ti.model, 'claude-opus-4');
  // And not one raw secret byte reached disk (the value was never walked).
  for (const leak of ['ghp_', 'AKIA', 'zzzzz', 'yyyyy', 'kkkkk', 'qqqqq', 'ppppp', 'ccccc', 'ttttt']) {
    assert.equal(raw.includes(leak), false, `${leak} must not appear on disk`);
  }
});

test('plural and camelCase-plural secret container keys redact like their singulars', (t) => {
  const { payload, raw } = captureOnce(t, {
    tool_input: {
      env: {
        api_keys: 'a'.repeat(20),
        tokens: 'b'.repeat(20),
        secrets: 'c'.repeat(20),
        credentials: 'd'.repeat(20),
        private_keys: 'e'.repeat(20),
        access_keys: 'f'.repeat(20),
        client_secrets: 'g'.repeat(20),
        API_KEYS: 'h'.repeat(20),
        TOKENS: 'i'.repeat(20),
        passwords: 'j'.repeat(20),
        region: 'us-east-1',
      },
      apiKeys: 'k'.repeat(20),        // camelCase plural — normalizes to api_Keys
      clientSecrets: 'l'.repeat(20),
    },
  });
  const env = payload.tool_input.env;
  for (const key of ['api_keys', 'tokens', 'secrets', 'credentials', 'private_keys',
    'access_keys', 'client_secrets', 'API_KEYS', 'TOKENS', 'passwords']) {
    assert.equal(env[key], '[redacted]', `${key} must redact`);
  }
  assert.equal(payload.tool_input.apiKeys, '[redacted]');
  assert.equal(payload.tool_input.clientSecrets, '[redacted]');
  assert.equal(env.region, 'us-east-1', 'sibling non-secret key keeps its value');
  for (const leak of ['aaaaa', 'bbbbb', 'kkkkk', 'lllll']) {
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
  // to the lone dot at every 'eyJ' start (~4.5s at this size, measured). The
  // fix is maskCompactTokens, a single-forward-pass scanner with no regex
  // engine: each 'eyJ' candidate consumes one segment walk and a confirmed
  // token is never re-scanned, so this input is linear. Capture runs
  // synchronously inside the hook handler, so this MUST stay fast.
  const evil = 'eyJ'.repeat(32000) + '.' + 'a'.repeat(1000); // ~97 KB
  const started = Date.now();
  const { raw } = captureOnce(t, { blob: evil }, { maxPayloadBytes: 97_000 });
  const elapsed = Date.now() - started;
  // The string is not a real JWT (only one dot), so it is not masked — the
  // point is purely that redaction returns promptly and a record is written.
  assert.ok(raw.length > 0, 'capture produced a line');
  assert.ok(elapsed < 2_000, `redaction must not hang; took ${elapsed}ms`);
});

test('a JWT with a header segment over 4096 chars is still masked, in capture and diagnostics', (t) => {
  // REGRESSION (BUG-135): the old bounded regex eyJ…{10,4096}\.…{10,4096}\.…{10,4096}
  // failed CLOSED the wrong way — a valid JWT whose protected header carries a
  // large x5c certificate chain (6,702 characters in the audited reproduction)
  // matched NOTHING, and the full credential reached hook-payloads.jsonl and
  // redactDiagnosticText verbatim. The scanner has no upper bound.
  const header = 'eyJ' + 'hbGciOiJSUzI1NiJ9'.repeat(400) + 'x5c'.repeat(800); // ~10 KB, all base64url
  const jwt = `${header}.${'p'.repeat(7000)}.${'s'.repeat(43)}`;
  assert.equal(jwt.length > 4096 * 2, true, 'fixture really is the audited shape');
  const { payload, raw } = captureOnce(t, { session: `auth=${jwt};` });
  assert.equal(payload.session, 'auth=[redacted];');
  assert.equal(raw.includes(header.slice(0, 64)), false, 'no prefix of the giant header may reach disk');
  assert.equal(raw.includes('pppppppppp'), false, 'no slice of the payload segment may reach disk');
  assert.equal(redactDiagnosticText(`token ${jwt} end`), 'token [redacted] end');
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

test('capture scrubs credentialed URLs that match no credential shape', (t) => {
  // BUG-136: a glpat- in URL userinfo and a ?private_token= query credential
  // match NO SECRET_VALUE_RES entry, so before the capture walk composed
  // scrubUrlCredentials they reached hook-payloads.jsonl verbatim.
  const token = 'glpat-AbCdEf1234567890';
  const { payload, raw } = captureOnce(t, {
    prompt: `clone https://luis:${token}@gitlab.com/o/r.git then push`,
    error: `fatal: unable to access 'https://host/o/r.git?private_token=${token}'`,
  });
  assert.equal(raw.includes(token), false, 'the token must not reach disk in any form');
  assert.equal(payload.prompt, 'clone https://[redacted]@gitlab.com/o/r.git then push');
  assert.equal(payload.error, "fatal: unable to access 'https://host/o/r.git?private_token=[redacted]'");
});

test('forge/API shapes git treats as secrets are masked by the SHARED scrubber, not just the git path', (t) => {
  // BUG-038: these prefixes lived only in exec.mjs's git-local extra list, so a
  // bare token relayed on a stalled spawn's pane (stallDiagnosticExcerpt applies
  // redactDiagnosticText + scrubUrlCredentials only) or captured in a hook
  // payload's free-text field survived into stall_detail, the board drawer,
  // /state, every /ws frame and hook-payloads.jsonl. The shapes are in the
  // shared SECRET_VALUE_RES now; this pins that they fire on BOTH consumers.
  const cases = [
    ['remote: the provided token (glpat-AbCdEf1234567890) is incorrect', 'glpat-'],
    [`remote: rejected glrt-${'r'.repeat(20)}`, 'glrt-'],
    [`remote: key AIza${'K'.repeat(35)} is not authorized`, 'AIza'],
    [`remote: sk-${'p'.repeat(32)} revoked`, 'sk-p'],
    [`remote: hf_${'h'.repeat(30)} expired`, 'hf_'],
    [`remote: dop_v1_${'d'.repeat(40)} deleted`, 'dop_v1_'],
  ];
  for (const [line, leak] of cases) {
    const scrubbed = redactDiagnosticText(line);
    assert.equal(scrubbed.includes(leak), false, `${leak} must be masked by redactDiagnosticText: ${scrubbed}`);
    assert.match(scrubbed, /\[redacted\]/);
  }
  // And the same shapes must not survive capture into hook-payloads.jsonl under
  // an innocent free-text key.
  const { raw } = captureOnce(t, {
    log: cases.map(([line]) => line).join('\n'),
  });
  for (const leak of ['glpat-', 'glrt-', 'AIza', 'sk-p', 'hf_', 'dop_v1_']) {
    assert.equal(raw.includes(leak), false, `${leak} must not reach the capture file`);
  }
  // The left boundary still protects ordinary prose from the generic sk- rule.
  const prose = 'disk-quota-exceeded-for-user on volume';
  assert.equal(redactDiagnosticText(prose), prose, 'innocent hyphenated words must survive verbatim');
});

test('bare forge shapes (glpat/sk-/AIza/hf_/dop_v1_) are masked on every shared surface', (t) => {
  // Until the shared shape list carried these, they were masked ONLY by
  // exec.mjs's git-local GIT_EXTRA_SECRET_RES — so a bare glpat leaked through
  // hook-payload capture, stallDiagnosticExcerpt's pane tail, and any future
  // diagnostic importing redactDiagnosticText, while CI stayed green. Assert the
  // SAME shapes on BOTH shared layers (the capture walk and redactDiagnosticText)
  // so the two can never silently drift apart again.
  const shaped = {
    gitlab: `remote: token glpat-AbCdEf1234567890 rejected`,
    google: `remote: key AIza${'K'.repeat(35)} is not authorized`,
    openai: `remote: sk-${'p'.repeat(32)} revoked`,
    hf: `remote: hf_${'h'.repeat(30)} expired`,
    do: `remote: dop_v1_${'d'.repeat(40)} is not valid`,
  };
  for (const [name, text] of Object.entries(shaped)) {
    assert.equal(redactDiagnosticText(text), text.replace(/(glpat|AIza|sk-|hf_|dop_v1_)[A-Za-z0-9_-]+/, '[redacted]'),
      `redactDiagnosticText must mask the ${name} shape`);
  }
  const { payload, raw } = captureOnce(t, shaped);
  for (const name of Object.keys(shaped)) {
    assert.equal(payload[name], shaped[name].replace(/(glpat|AIza|sk-|hf_|dop_v1_)[A-Za-z0-9_-]+/, '[redacted]'),
      `capture must mask the ${name} shape`);
  }
  for (const leak of ['glpat-AbCdEf', 'AIza' + 'K'.repeat(10), 'sk-' + 'p'.repeat(10), 'hf_' + 'h'.repeat(10), 'dop_v1_' + 'd'.repeat(10)]) {
    assert.equal(raw.includes(leak), false, `${leak} must not appear on disk`);
  }
  // The sk- left boundary, pinned at the shared layer too: the generic rule must
  // NOT fire inside an ordinary hyphenated word.
  assert.equal(redactDiagnosticText('disk-quota-exceeded-for-user'), 'disk-quota-exceeded-for-user');
});

test('redactDiagnosticText still has no USERINFO rule — credentialed URLs need scrubUrlCredentials', () => {
  // What the shape list does NOT cover, pinned deliberately. A credentialed URL's
  // userinfo is POSITIONAL, not shaped — `https://luis:hunter2@host` matches no
  // credential shape — so scrubUrlCredentials stays a separate export that
  // callers compose explicitly, as gitStderrDetail and stallDiagnosticExcerpt do
  // (and, since BUG-136, the capture walk's textWithinBudget). If a future
  // reader folds a userinfo pattern into SECRET_VALUE_RES instead, this fails
  // and points at the capture-format cases above that would then need
  // revisiting.
  const line = "fatal: unable to access 'https://luis:hunter2-password@gitlab.com/x/y.git/'";
  assert.equal(redactDiagnosticText(line), line);
  // But a SHAPED token inside userinfo is now masked in place by the shape pass
  // alone (the URL wrapper survives). scrubUrlCredentials additionally removes
  // the whole userinfo including the username half.
  const shaped = "fatal: unable to access 'https://luis:glpat-AbCdEf1234567890@gitlab.com/x/y.git/'";
  assert.equal(redactDiagnosticText(shaped), "fatal: unable to access 'https://luis:[redacted]@gitlab.com/x/y.git/'");
  assert.equal(scrubUrlCredentials(shaped), "fatal: unable to access 'https://[redacted]@gitlab.com/x/y.git/'");
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
