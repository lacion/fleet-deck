// tests/git-stderr-detail.test.mjs — the "git's remedy reaches the board, git's
// credentials never do" promise made executable.
//
// A repo-mode spawn whose clone fails used to show ONE distilled line on the
// card. On a Coder workspace that line read "fatal: Could not read from remote
// repository." while the two lines git printed directly above it — the
// workspace's public key and the settings/ssh/new URL to register it — were the
// entire fix, and were discarded. gitStderrDetail keeps them. But git stderr
// also echoes clone URLs, and a clone URL can be
// `https://user:token@host/repo.git`, so the same excerpt is the most direct
// route a credential has ever had into the UI. Both halves are pinned here: the
// diagnostic must survive, the secret must not — and, following the convention
// of tests/payload-redaction.test.mjs, "must not" is asserted on the RAW bytes
// of the whole string, because a redacted-looking field proves nothing if the
// secret survives elsewhere on the line.

import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { distillGitStderr, gitStderrDetail, redactGitText } from '../scripts/fleetd/exec.mjs';
import { scrubUrlCredentials } from '../scripts/fleetd/payload-capture.mjs';
import { openDb } from '../scripts/fleetd/db.mjs';

// The incident, verbatim in shape: narration, remedy, verdict.
const CODER_CLONE_STDERR = [
  "Cloning into '/home/coder/projects/fleetdeck'...",
  'Coder: this workspace authenticates to git with the key below.',
  'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFakeKeyForTestsOnly0000000000000 coder@workspace',
  'Add it at https://github.com/settings/ssh/new before cloning a private repo.',
  'git@github.com: Permission denied (publickey).',
  'fatal: Could not read from remote repository.',
  '',
].join('\n');

test('the Coder incident: the remedy above the verdict survives into the detail', () => {
  const detail = gitStderrDetail(CODER_CLONE_STDERR);
  assert.match(detail, /https:\/\/github\.com\/settings\/ssh\/new/,
    'the URL the human has to visit is the whole point of the field');
  assert.match(detail, /ssh-ed25519 AAAAC3NzaC1lZDI1NTE5/,
    'the public key to paste there must survive too');
  assert.match(detail, /fatal: Could not read from remote repository\./,
    'the verdict stays as the last line — the detail is a superset of the note');
});

test('the note is unchanged: still only the distilled verdict', () => {
  // The no-regression half. Routing stderr through the credential scrub first
  // must not disturb the note for any stderr without `scheme://userinfo@` — i.e.
  // for everything the suite already pins.
  const note = distillGitStderr(scrubUrlCredentials(CODER_CLONE_STDERR));
  assert.equal(note, 'fatal: Could not read from remote repository.');
  assert.doesNotMatch(note, /Cloning into/, 'narration must never be the note');
  assert.doesNotMatch(note, /settings\/ssh\/new/, 'the note stays a one-liner; the detail carries the rest');
});

test('URL userinfo never reaches the detail — the credential control', () => {
  const cases = [
    // A GitLab PAT in the password half: matches NO entry in the shape list, so
    // the positional scrub is the only thing standing between it and the board.
    { in: "fatal: unable to access 'https://luis:glpat-AbCdEf1234567890@gitlab.com/x/y.git/'",
      leak: 'glpat-AbCdEf1234567890', keep: '[redacted]@gitlab.com' },
    // Bare-token form: no username at all, so the WHOLE userinfo must go.
    { in: "fatal: unable to access 'https://ghs_tokenvalue123456@github.com/x.git'",
      leak: 'ghs_tokenvalue123456', keep: '[redacted]@github.com' },
    // ssh:// carries userinfo just as happily as https.
    { in: 'fatal: connect to ssh://deploy:p4ssw0rd-secret@host.example/x failed',
      leak: 'p4ssw0rd-secret', keep: '[redacted]@host.example' },
  ];
  for (const { in: input, leak, keep } of cases) {
    const detail = gitStderrDetail(input);
    assert.doesNotMatch(detail, new RegExp(leak.replace(/[.*+?^${}()|[\]\\-]/g, '\\$&')),
      `${leak} must not survive anywhere in the excerpt`);
    assert.ok(detail.includes(keep), `expected ${keep} in: ${detail}`);
  }
});

test('a literal @ inside a password collapses on the LAST @, not the first', () => {
  // The realistic mangling case. Collapsing on the FIRST '@' would leave
  // `ss@host` looking like the hostname AND leave half the password in place.
  const detail = gitStderrDetail("fatal: unable to access 'https://user:p@ss@host.example/x.git'");
  assert.ok(detail.includes('https://[redacted]@host.example/x.git'), detail);
  assert.doesNotMatch(detail, /p@ss/, 'no fragment of the password may survive');
});

test('scp-style git@host:path survives verbatim', () => {
  // That form has no password slot, so a username is not a credential — and
  // mangling it would destroy exactly the legibility the incident needed, since
  // the Coder remedy block quotes ssh URLs of this shape.
  const detail = gitStderrDetail('git@github.com: Permission denied (publickey).\nfatal: Could not read from remote repository.');
  assert.ok(detail.includes('git@github.com:'), detail);
});

test('a URL with no userinfo is left completely alone', () => {
  const detail = gitStderrDetail('Add it at https://github.com/settings/ssh/new now');
  assert.equal(detail, 'Add it at https://github.com/settings/ssh/new now');
});

test('known credential shapes in git stderr are masked', () => {
  const jwt = 'eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJVadQssw5c';
  const detail = gitStderrDetail([
    `remote: log line with sk-ant-api03-${'A'.repeat(40)} in it`,
    `remote: and ghp_${'b'.repeat(36)} too`,
    `remote: and github_pat_${'c'.repeat(30)} as well`,
    `remote: session ${jwt} expired`,
    `remote: sent Authorization: Bearer ${'d'.repeat(40)}`,
    'fatal: authentication failed',
  ].join('\n'));
  for (const leak of ['sk-ant-', 'ghp_', 'github_pat_', 'eyJhbGci', 'Bearer d']) {
    assert.equal(detail.includes(leak), false, `${leak} must be masked`);
  }
  assert.equal(detail.match(/\[redacted\]/g).length, 5);
  assert.match(detail, /fatal: authentication failed/);
});

test('an exact secret with no known shape is scrubbed via the caller-supplied list', () => {
  // The path that catches a corporate password or a forge PAT that matches no
  // shape at all and appears BARE, with no URL around it to key off.
  const secret = 'corp-p4ss-not-a-known-shape';
  const detail = gitStderrDetail(`remote: HTTP Basic: Access denied for user '${secret}'`, { secrets: [secret] });
  assert.ok(detail.includes('[redacted]'), detail);
  assert.equal(detail.includes(secret), false, 'not one byte of the supplied secret may survive');
  assert.equal(detail.includes('p4ss'), false, 'nor any distinctive fragment of it');
});

test('the excerpt is bounded to 2000 bytes', () => {
  const flood = Array.from({ length: 500 }, (_, i) => `remote: line ${i} ${'x'.repeat(200)}`).join('\n');
  const detail = gitStderrDetail(flood);
  assert.ok(Buffer.byteLength(detail) <= 2000, `byte cap: got ${Buffer.byteLength(detail)}`);
  // Tail, not head: git narrates first and the remedy sits above the verdict.
  assert.match(detail, /line 499/);
  assert.doesNotMatch(detail, /line 0 /);
});

test('the excerpt is bounded to 20 LINES, independently of the byte cap', () => {
  // The fixture above cannot pin the line cap and never could: 20 lines of 200
  // characters is ~4300 bytes, so the BYTE cap alone trims it to ~10 lines and a
  // `<= 20` assertion holds no matter what GIT_DETAIL_LINES says — deleting the
  // slice entirely left the suite green. SHORT lines are what make the line cap the
  // only binding constraint: 2000 bytes would permit ~200 of these, so an EXACT
  // count fails the moment the slice goes away or the constant moves.
  const detail = gitStderrDetail(Array.from({ length: 500 }, (_, i) => `remote: ${i}`).join('\n'));
  assert.equal(detail.split('\n').length, 20, 'exactly GIT_DETAIL_LINES lines, not merely "at most"');
  assert.ok(Buffer.byteLength(detail) < 2000, 'the byte cap must NOT be what bound this fixture');
  assert.match(detail, /remote: 499$/, 'still the tail');
});

test('an over-long URL userinfo FAILS CLOSED instead of passing through', () => {
  // The bounded userinfo class does not TRUNCATE a longer credential — it simply
  // stops matching, and pre-fix a 600-character token passed through verbatim into
  // fail_detail, the card note, the ticker, the durable SpawnFailed event and
  // /state, i.e. every surface this field exists to protect. Redacting the whole
  // over-long authority (host included) is the accepted cost.
  const token = 'T'.repeat(600);
  const line = `fatal: unable to access 'https://user:${token}@gitlab.com/x.git'`;
  for (const out of [scrubUrlCredentials(line), gitStderrDetail(line), distillGitStderr(scrubUrlCredentials(line))]) {
    assert.equal(out.includes(token), false, 'the over-long token must not survive');
    assert.equal(out.includes('TTTTTTTT'), false, 'nor any run of it');
    assert.ok(out.includes('[redacted]'), out);
  }
});

test('a credential in the URL QUERY STRING or FRAGMENT is redacted too', () => {
  // The layers keyed on `scheme://userinfo@` structurally cannot see this: their
  // classes stop at `?` by design. `?access_token=` (Gitea), `?private_token=` and
  // `?job_token=` (GitLab CI) are real bare-token URL forms, and a CI job token
  // matches no known credential shape either.
  const cases = [
    ["fatal: unable to access 'https://gitea.example/o/r.git?access_token=deadbeefcafebabe0123': 401", 'deadbeefcafebabe0123'],
    ["fatal: unable to access 'https://gitlab.example/o/r.git?private_token=abcd1234abcd1234': 401", 'abcd1234abcd1234'],
    ["fatal: repository 'https://host/r.git#auth_key=zzzzyyyyxxxxwwww' not found", 'zzzzyyyyxxxxwwww'],
  ];
  for (const [line, leak] of cases) {
    const detail = gitStderrDetail(line);
    assert.equal(detail.includes(leak), false, `${leak} must not survive: ${detail}`);
    assert.ok(detail.includes('[redacted]'), detail);
  }
  // …and a parameter that is plainly not a credential is left legible.
  assert.equal(scrubUrlCredentials('see https://host/r.git?ref=main now'), 'see https://host/r.git?ref=main now');
});

test('a SCHEMELESS user:token@host pair is redacted, scp-style is not', () => {
  const out = scrubUrlCredentials("fatal: could not read from 'user:tok3nvalue1234@host.example/x.git'");
  assert.equal(out.includes('tok3nvalue1234'), false, out);
  assert.ok(out.includes("'[redacted]@host.example/x.git'"), out);
  // The colon requirement is exactly what keeps the scp form — whose legibility is
  // the reason this whole expander exists — untouched.
  assert.equal(scrubUrlCredentials('git@github.com: Permission denied (publickey).'),
    'git@github.com: Permission denied (publickey).');
});

test('WHITESPACE inside the password half does not defeat the scrub', () => {
  // fleetd rejects such an origin for a clone, but materializeBranch's fetch reads
  // its remote from the CHECKOUT's .git/config, which fleetd never validated.
  const out = scrubUrlCredentials("fatal: unable to access 'https://user:my pass phrase@host.example/x.git/'");
  assert.equal(out.includes('my pass phrase'), false, out);
  assert.ok(out.includes('https://[redacted]@host.example/x.git/'), out);
  // The guard on that tolerance: prose with a colon and a later @ must survive,
  // which is why the run BEFORE the colon still forbids whitespace.
  const prose = 'remote: see https://gitlab.example for help: contact admin@example.com';
  assert.equal(scrubUrlCredentials(prose), prose);
});

test('a PRIVATE KEY BLOCK longer than the line cap is masked, not tail-sliced', () => {
  // Pipeline order. Redaction used to run AFTER `lines.slice(-20)`, so a key block
  // longer than the cap lost its `-----BEGIN` marker in the DROPPED HEAD and the
  // surviving base64 body lines were emitted verbatim — the PEM rule's `…|$`
  // tolerance is for the BYTE cap cutting the END, not for a missing BEGIN.
  const body = Array.from({ length: 32 }, (_, i) => `AAAAB3NzaC1yc2EAAAADAQABAAABgQSECRETKEYLINE${i}`);
  const detail = gitStderrDetail([
    'remote: askpass returned:',
    '-----BEGIN OPENSSH PRIVATE KEY-----',
    ...body,
    '-----END OPENSSH PRIVATE KEY-----',
    'fatal: Could not read from remote repository.',
  ].join('\n'));
  assert.equal(detail.includes('SECRETKEYLINE'), false, `no key body may survive: ${detail}`);
  assert.equal(detail.includes('AAAAB3NzaC1yc2E'), false, 'not one base64 line of it');
  assert.match(detail, /fatal: Could not read from remote repository\./, 'the verdict still survives');
});

test('a bare GitLab/Google/OpenAI token with no URL around it is masked', () => {
  // The fetch path has no origin userinfo to derive an exact needle from, and
  // SECRET_VALUE_RES carries none of these prefixes, so without the git-local
  // shape list a bare forge token on a `remote:` line had NO covering layer.
  const cases = [
    `remote: the provided token (glpat-AbCdEf1234567890) is incorrect`,
    `remote: rejected glrt-${'r'.repeat(20)}`,
    `remote: key AIza${'K'.repeat(35)} is not authorized`,
    `remote: sk-${'p'.repeat(32)} revoked`,
    `remote: hf_${'h'.repeat(30)} expired`,
  ];
  for (const line of cases) {
    const detail = gitStderrDetail(`${line}\nfatal: authentication failed`);
    for (const leak of ['glpat-', 'glrt-', 'AIza', 'sk-p', 'hf_h']) {
      assert.equal(detail.includes(leak), false, `${leak} must be masked in: ${detail}`);
    }
    assert.match(detail, /fatal: authentication failed/);
  }
});

test('the cap counts BYTES, and never leaves a half-decoded character behind', () => {
  // A stderr of CJK would blow a 2000-CODE-UNIT budget to ~6000 bytes in
  // SQLite and in every /ws frame. The byte tail can also start mid-character,
  // which must be dropped rather than shipped as U+FFFD.
  const cjk = Array.from({ length: 60 }, (_, i) => `remote: 第${i}行${'漢'.repeat(80)}`).join('\n');
  const detail = gitStderrDetail(cjk);
  assert.ok(Buffer.byteLength(detail) <= 2000, `byte cap: got ${Buffer.byteLength(detail)}`);
  assert.ok(detail.length < 2000, 'a code-unit cap would have allowed far more than this');
  assert.equal(detail.startsWith('�'), false, 'the partial-character fixup must have run');
});

test('a 4-byte astral tail is cut on a character boundary, not inside a surrogate pair', () => {
  // The sharper half of the byte cap. CJK above is 3 bytes per character; an
  // emoji is 4 bytes AND two JS code units, so a byte tail can land both
  // mid-sequence (→ U+FFFD) and between the halves of a surrogate pair (→ a lone
  // surrogate, which JSON.stringify happily emits and which then rides every /ws
  // frame as an unpaired \ud83d). Progress spinners and CI bots put emoji in
  // `remote:` text routinely, so this is not a contrived input.
  const emoji = Array.from({ length: 60 }, (_, i) => `remote: step ${i} ${'🚀'.repeat(60)}`).join('\n');
  const detail = gitStderrDetail(emoji);
  assert.ok(Buffer.byteLength(detail) <= 2000, `byte cap: got ${Buffer.byteLength(detail)}`);
  assert.equal(detail.includes('�'), false, 'no replacement character may survive anywhere');
  assert.equal(detail.startsWith('�'), false, 'the partial-character fixup must have run');
  // No lone surrogate: a well-formed string survives a JSON round-trip byte for
  // byte, which is exactly the trip the snapshot makes to every board.
  assert.equal(JSON.parse(JSON.stringify(detail)), detail, 'the excerpt must be well-formed UTF-16');
  assert.equal(/[\uD800-\uDFFF]/.test(detail.replace(/[\uD800-\uDBFF][\uDC00-\uDFFF]/g, '')), false,
    'an unpaired surrogate would be a lone half of an emoji');
  assert.match(detail, /step 59/, 'still the tail, not the head');
});

test('capping runs after redaction, so a cut can never re-expose a secret', () => {
  // A credentialed URL placed so the byte tail lands mid-token: masking first
  // means the cap only ever removes bytes from an already-safe string.
  const secret = 'glpat-ZZZZZZZZZZZZZZZZZZZZ';
  const detail = gitStderrDetail([
    `remote: padding ${'p'.repeat(1900)}`,
    `fatal: unable to access 'https://ci:${secret}@gitlab.com/x/y.git/'`,
  ].join('\n'));
  assert.equal(detail.includes(secret), false);
  assert.equal(detail.includes('glpat-'), false, 'not even a truncated prefix of the token');
  assert.ok(Buffer.byteLength(detail) <= 2000);
});

test('empty, blank and non-string inputs are null, not an empty string', () => {
  // null is the field's "nothing was captured" value; '' would render an empty
  // expander on the card.
  for (const input of ['', '   ', '\n\n  \n', null, undefined, 42, {}, []]) {
    assert.equal(gitStderrDetail(input), null, `${JSON.stringify(input)} must be null`);
  }
});

test('carriage returns and C0/C1 control characters are stripped', () => {
  // git stderr relays REMOTE-CONTROLLED `remote:` text into a board <pre> AND, via
  // the drawer's copy button, into an operator's clipboard. An escape sequence
  // reaching either is the whole reason this strip exists — and the clipboard is
  // why C1 counts: U+009B is a one-byte CSI, inert in HTML but not in a terminal
  // with bracketed paste off. U+2028/2029 ride along for the same reason.
  const detail = gitStderrDetail('remote: \x1b[31mred\x1b[0m\r\nremote: bell\x07here\r\nremote: csi\x9b31mx\u2028y\r\nfatal: no');
  assert.equal(detail.includes('\x1b'), false, 'ESC must not survive');
  assert.equal(detail.includes('\x07'), false, 'BEL must not survive');
  assert.equal(detail.includes('\r'), false, 'CR must not survive');
  assert.equal(detail.includes('\x9b'), false, 'C1 CSI must not survive');
  assert.equal(detail.includes('\u2028'), false, 'LINE SEPARATOR must not survive');
  assert.match(detail, /remote: \[31mred\[0m/);
  assert.match(detail, /remote: bellhere/);
  assert.match(detail, /remote: csi31mxy/);
  assert.match(detail, /fatal: no/);
});

test('TAB survives: a tab-indented remedy block must not have its words joined', () => {
  // The strip class deliberately excludes \x09. Deleting it (rather than keeping or
  // widening it) silently joined words in server-relayed `remote:` hint blocks,
  // which are routinely tab-indented — and the board renders this in a <pre>,
  // where a literal tab is exactly right.
  const detail = gitStderrDetail('remote: hint\tuse ssh\nfatal: no');
  assert.equal(detail, 'remote: hint\tuse ssh\nfatal: no');
  assert.equal(detail.includes('hintuse'), false, 'the tab must not be deleted, joining two words');
});

test('trailing whitespace and blank margins are trimmed, inner blanks kept', () => {
  const detail = gitStderrDetail('\n\nremote: one   \n\nremote: two\t\n\n');
  assert.equal(detail, 'remote: one\n\nremote: two');
});

test('the NOTE and the DETAIL are hardened by the same pass — no weaker sink', () => {
  // The defect this pairing exists to prevent: the note used to receive only the
  // positional URL scrub, so a shape-matched or exact-needle secret was masked in
  // the expander and printed verbatim in the note six characters away. The note is
  // the WORSE sink — card, 120-char ticker, HTTP 409 body, and the durable
  // SpawnFailed event that outlives the archived card.
  const cases = [
    { stderr: `fatal: helper rejected token ghp_${'A'.repeat(30)}`, leak: 'ghp_A', secrets: [] },
    { stderr: "fatal: Authentication failed for user 'hunter2secretpw'", leak: 'hunter2secretpw', secrets: ['hunter2secretpw'] },
    { stderr: 'fatal: rejected glpat-AbCdEf1234567890 by the server', leak: 'glpat-', secrets: [] },
  ];
  for (const { stderr, leak, secrets } of cases) {
    const hardened = redactGitText(stderr, secrets);
    const note = distillGitStderr(hardened);
    const detail = gitStderrDetail(hardened, { secrets });
    assert.equal(note.includes(leak), false, `the NOTE leaked ${leak}: ${note}`);
    assert.equal(detail.includes(leak), false, `the DETAIL leaked ${leak}: ${detail}`);
    assert.ok(note.includes('[redacted]'), note);
  }
});

test('the composed credential scrub stays effectively linear on adversarial input', () => {
  // Five layers now, each with bounded runs; the ReDoS audit in payload-capture.mjs
  // is only worth what a measurement says. git stderr is attacker-influenceable
  // (`remote:` text) and arrives up to execFile's ~1 MB buffer.
  const probes = [
    `https://${'a'.repeat(60_000)}`,                    // the original quadratic scheme run
    `https://${'a:'.repeat(30_000)}`,                   // colon-dense authority (LAYER 2 backtracking)
    `${'x:'.repeat(30_000)}y`,                          // colon-dense with no @ (LAYER 4 backtracking)
    `https://h/r.git?${'tok='.repeat(15_000)}`,         // param-dense query (LAYER 5 start positions)
    `${'a://'.repeat(15_000)}`,                         // many scheme starts
  ];
  for (const probe of probes) {
    const started = Date.now();
    scrubUrlCredentials(probe);
    const elapsed = Date.now() - started;
    assert.ok(elapsed < 1_000, `scrubbing must not hang; ${probe.slice(0, 12)}… took ${elapsed}ms`);
  }
});

test('gitStderrDetail is idempotent on a credentialed input', () => {
  // Callers scrub at more than one layer by design, so a second pass must be a
  // no-op rather than corrupting `[redacted]@` into something else.
  const input = "remote: denied\nfatal: unable to access 'https://u:tok-1234567890@h/x.git/'";
  const once = gitStderrDetail(input);
  assert.equal(gitStderrDetail(once), once);
});

test('an adversarial scheme-shaped run cannot stall the daemon (ReDoS guard)', () => {
  // Pre-fix the scheme run was unbounded: `[a-z][a-z0-9+.-]*:\/\/` on a long
  // alphanumeric string matched to the end and backtracked hunting for `://` at
  // EVERY start position → O(n^2), a measured 3.4s on 60 KB of 'a'. git stderr
  // is attacker-influenceable (`remote:` text) and arrives up to execFile's ~1 MB
  // buffer, so this MUST stay linear. Bounding the scheme to {0,32} did it.
  const evil = `https://${'a'.repeat(60_000)}`;
  const started = Date.now();
  const detail = gitStderrDetail(evil);
  const elapsed = Date.now() - started;
  assert.ok(detail.length > 0, 'a value is still produced');
  assert.ok(elapsed < 1_000, `scrubbing must not hang; took ${elapsed}ms`);
});

test('a legacy spawns table gains fail_detail additively, backfilled NULL', (t) => {
  // The schema half of the change, on the mechanism this DB already uses: a
  // guarded PRAGMA-driven ALTER in migrate(), because an existing DB never
  // re-runs the CREATE TABLE that carries the column for fresh installs.
  const dir = mkdtempSync(path.join(tmpdir(), 'fd-faildetail-db-'));
  const file = path.join(dir, 'fleet.db');
  t.after(() => rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const legacy = new DatabaseSync(file);
  legacy.exec(`CREATE TABLE spawns (
    spawn_id TEXT PRIMARY KEY, session_id TEXT, callsign TEXT,
    tmux_session TEXT, tmux_window TEXT, cwd TEXT, worktree_path TEXT,
    requested_at INTEGER, status TEXT DEFAULT 'spawning',
    skip_permissions INTEGER DEFAULT 0, stall_detail TEXT
  );
  INSERT INTO spawns (spawn_id, session_id, callsign, requested_at, status)
    VALUES ('legacy', 'sid', 'otter', 1, 'gone')`);
  legacy.close();

  const db = openDb(file);
  t.after(() => db.close());
  const columns = db.prepare('PRAGMA table_info(spawns)').all().map(row => row.name);
  assert.ok(columns.includes('fail_detail'), 'the column must be added to an existing table');
  assert.equal(db.prepare("SELECT fail_detail FROM spawns WHERE spawn_id = 'legacy'").get().fail_detail, null,
    'NULL is the truthful backfill — no detail was ever captured for that row');

  // Idempotent: openDb runs migrate() on every daemon start, so a second pass
  // must not attempt the ALTER again (SQLite would throw "duplicate column").
  const again = openDb(file);
  again.close();
  const twice = db.prepare('PRAGMA table_info(spawns)').all().filter(row => row.name === 'fail_detail');
  assert.equal(twice.length, 1);
});
