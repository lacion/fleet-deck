# Fleet Deck v2 security audit

- **Audit date:** 2026-10-14
- **Revision:** `2ac95b7f763757150c252186d2b3acba66509abf`
- **Result:** 0 critical, 0 high, 3 medium, and 2 low findings

## Executive summary

Fleet Deck v2 has a strong security baseline. Network links use pinned mutual TLS and transcript-bound identities; local sockets verify the peer UID; the board applies principal, scope, ownership, CSRF, and sharing checks; coordinator ingress is loopback-only and tightly bounded; credential values are redacted and generally kept out of persistent state; filesystem reads defend against traversal, symlink replacement, and hard links; and artifact documents run in a sandboxed origin. The Rust workspace forbids unsafe code.

The review found no critical or high-severity issue. The most important work is availability hardening on the board's network and login ingress, followed by closing a known gap where Git can read a tracked hard link without applying the inode-based private-file check used by direct file operations. Two lower-severity gaps concern redirected tracker asset requests and detached descendants of scheduled prechecks.

This is a source-level security review with automated test and dependency checks. It is not a claim that the software is vulnerability-free, and it did not include an external penetration test or live tests against third-party providers.

## Scope and method

The review covered the production and supporting code in `crates/`, `web/src/`, `tools/`, `packaging/`, `spikes/`, the Cargo and Bun dependency manifests, CI configuration, and the security design documentation. The existing `docs/v2/SECURITY-CHECKLIST.md` was used as an inventory, then the implementation was traced independently.

The following trust boundaries and vulnerability classes were examined:

- board HTTPS and WebSocket ingress, login, cookies, CSRF, Origin handling, bearer scopes, owner-only routes, and member sharing filters;
- deck-to-station networking, pairing, certificate pinning, identity binding, replay and epoch handling, framing, flow control, and timeouts;
- local Unix sockets, peer credentials, permissions, parsing limits, and secret-bearing error paths;
- coordinator/MCP ingress, credential binding, rate and concurrency limits, host checks, request parsing, and session lifecycle;
- provider and shell process launch, environment inheritance, command construction, cleanup, and secret redaction;
- file browsing, search, Git diffs, checkpoints, restore, artifacts, private paths, path traversal, symlinks, and hard links;
- external integrations, OAuth and device flows, token storage, HTTP construction, redirects, response limits, and error scrubbing;
- React rendering, Markdown, links, terminal output, artifact isolation, and browser security headers;
- SQLite queries and migrations, event replay, idempotency, recovery, updates, packaging, and dependency policy.

Severity reflects practical impact and exploit prerequisites in Fleet Deck's intended deployment. Private LAN and tailnet peers are treated as potentially hostile. A compromised OS account or root is outside the boundary, but provider processes and repository content are treated as untrusted where the product already attempts to mediate them.

## Findings

### FDV2-001 — Board connections have no post-TLS admission cap or header timeout

- **Severity:** Medium
- **Status:** Open
- **Impact:** Remote denial of service from an address allowed by the D13 network policy

The board gives each TLS handshake a ten-second timeout, but after a handshake succeeds it spawns an HTTP/1 connection task with no connection semaphore, request-header timeout, or idle lifetime. A peer on an allowed LAN or tailnet can complete TLS and then hold many connections open while sending no headers or sending them slowly. Each connection consumes a task and file descriptor indefinitely. A sufficiently large set can prevent legitimate browser and API use or exhaust the process.

Evidence:

- `crates/fleetdeck-deck/src/http/mod.rs:559-595` accepts every TCP connection into an unbounded `JoinSet` and awaits `serve_connection` without a post-handshake timeout.
- `crates/fleetdeck-link/src/node.rs:348-369` also spawns one task per accepted link without a semaphore. Link handshakes are time-bounded and require pinned mTLS before use, so this is a smaller exposure, but a private-network peer can still create bursts of pending tasks.
- The coordinator ingress already demonstrates the desired pattern: bounded connections and unauthenticated work plus explicit header, body, and size limits.

Recommended remediation:

1. Acquire a bounded connection permit immediately after `accept`, before TLS work, and keep it for the connection lifetime.
2. Configure a request-header read timeout and an idle/keep-alive timeout for board HTTP connections.
3. Apply a smaller unauthenticated admission pool so authenticated traffic retains capacity during a flood.
4. Add equivalent pending-handshake admission limits to the link listener.
5. Add slow-header and connection-flood tests that assert bounded task and descriptor growth and continued service for a legitimate request.

### FDV2-002 — Login throttling permits global lockout and concurrent password-hash flooding

- **Severity:** Medium
- **Status:** Open
- **Impact:** Login denial of service and CPU exhaustion without credentials

All login methods share one process-wide queue of failed attempts. Five bad attempts in sixty seconds block every new password, login-link, and join attempt. Any peer that can reach the board can maintain that lockout with a small request rate. Existing sessions continue to work, but a logged-out owner cannot sign in while the attack continues.

The limiter is checked before password verification and records a failure only after verification completes. Concurrent requests can therefore all pass the initial check and start Argon2 verification in the blocking pool. This defeats the intended five-attempt bound for in-flight requests and can consume CPU and blocking-worker capacity. FDV2-001 makes this easier because board connections are also unbounded.

Evidence:

- `crates/fleetdeck-deck/src/http/auth_routes.rs:29-59` implements one global `VecDeque` and locks after five recorded failures.
- `crates/fleetdeck-deck/src/http/auth_routes.rs:193-221` checks the limiter, awaits password verification, and records failure later in `login` at lines 156-188.
- `crates/fleetdeck-deck/src/store/auth.rs:245-254` runs Argon2 verification as blocking work.

Recommended remediation:

1. Reserve an attempt atomically before expensive verification and release or classify it after completion.
2. Put a small semaphore around password hashing, with a bounded queue or immediate `429` response.
3. Use both per-source and global token buckets. Keep the global limit high enough that one peer cannot lock out all users.
4. Separate password failures from single-use login-link and member-invite failures so one flow cannot disable all others.
5. Add parallel-login tests that prove hashing concurrency is capped and one source cannot lock out a second source.

### FDV2-003 — A tracked hard link to a private file can enter checkpoints and diffs

- **Severity:** Medium
- **Status:** Fixed in 1d9bcbf1: changed tracked paths are staged from the opened file (no `git add -u`), a private file is left out of the snapshot, and checkpoints without `capture: 2` are not shown to members or restored.
- **Impact:** Disclosure of local credentials through a session diff or retained Git checkpoint

Direct file reads, search, and artifact staging inspect the opened file's device, inode, and link count. They refuse known credential inodes and conservatively refuse regular files with more than one hard link. The tracked Git snapshot path does not apply that check. It asks Git to update a private index from all tracked paths; the later diff excludes private *names* only.

A provider process or local process that can modify a workspace can replace an already tracked path with a hard link to a credential readable by the station account. The snapshot reads that inode through Git, writes its contents into a Git object, and the session diff can return it under the harmless tracked pathname. A member who is allowed to view that session may then receive the credential. The actor needs permission to create the hard link, and filesystem protections such as `fs.protected_hardlinks` can block links to files the actor does not own; those constraints reduce but do not remove the issue for same-user credentials.

Evidence:

- `crates/fleetdeck-station/src/workspace/private_paths.rs:294-312` contains the inode and link-count checks used by direct operations.
- `crates/fleetdeck-station/src/workspace/private_paths.rs:321-340` builds Git exclusions from path names.
- `crates/fleetdeck-station/src/workspace/checkpoint.rs:354-369` snapshots all tracked changes with `git add -u` into a private index and has no `PrivatePaths` input.
- `crates/fleetdeck-station/src/workspace/session_diff.rs:145-207` snapshots first, then passes only name-based exclusions to `git diff-tree`.

Recommended remediation:

1. Before `git add -u`, enumerate tracked paths, open each changed regular file without following symlinks, and apply `is_private_opened` to the opened metadata.
2. Fail the snapshot closed, or explicitly exclude the offending tracked path from both the private index and every diff, when a private inode or non-Git multi-link file is found.
3. Ensure restore and checkpoint consumers cannot reintroduce content captured by an older unsafe checkpoint.
4. Add a regression test that replaces an already tracked file with a hard link to a registered credential and asserts that neither a Git object, owner diff, nor member-filtered diff contains the secret.

### FDV2-004 — Tracker asset redirects are not checked against the host allowlist

- **Severity:** Low
- **Status:** Open
- **Impact:** Limited server-side request forgery through an allowed upstream redirect

The tracker asset loader validates that the initial URL is HTTPS and belongs to the configured Jira host or `uploads.linear.app`. It then lets curl follow up to three redirects to any HTTPS destination. A caller who can identify an open redirect on an allowed host can make the station request an otherwise disallowed HTTPS URL. If that destination presents a trusted certificate and returns a PNG, JPEG, GIF, or WebP body of at most 5 MiB, the response is returned as base64.

The audited curl 8.22.0 runtime explicitly strips `Authorization` and `Cookie` on cross-origin redirects, and the code does not enable `--location-trusted`. This finding is therefore not a cross-host credential leak on the audited runtime. HTTPS-only redirects, certificate validation, content sniffing, and the response-size cap also limit impact.

Evidence:

- `crates/fleetdeck-station/src/integrations/asset.rs:25-54` validates only the first host and sets `follow: true`.
- `crates/fleetdeck-station/src/integrations/http.rs:45-59` configures `location`, three redirects, and `proto-redir = "=https"` without validating each `Location` target.
- `crates/fleetdeck-station/src/integrations/tracker.rs:261-282` supplies the tracker authorization header for the initial allowed host.

Recommended remediation:

Disable automatic redirects for authenticated asset requests. Follow redirects in application code, parse and validate every absolute target against the binding's host allowlist, and attach authorization only after that validation. If broader redirect support is intentional, enforce a separate destination policy that rejects loopback, link-local, and private addresses after each DNS resolution.

### FDV2-005 — Scheduled prechecks can leave detached descendants running

- **Severity:** Low
- **Status:** Open; also acknowledged in `docs/v2/SECURITY-CHECKLIST.md`
- **Impact:** A timed-out or stopped precheck can retain a background process

Prechecks run in their own process group, with a timeout and TERM/KILL cleanup of that group. A command can create a new session or process group and escape that cleanup. The setup-command runner has marker-based cleanup for detached children, but the precheck runner does not.

Prechecks are owner-created, require `allow_setup_cmd`, have a sixty-second limit, and receive a cleared, explicit environment. Those prerequisites keep severity low. The gap still matters when a trusted command invokes repository-controlled tooling that detaches unexpectedly or maliciously.

Evidence:

- `crates/fleetdeck-station/src/workspace/precheck.rs:138-225` creates and kills one process group only.
- `crates/fleetdeck-station/src/workspace/setup.rs` implements marker cleanup for detached setup descendants and provides a model for the fix.

Recommended remediation:

Give each precheck an unguessable process marker and apply the setup runner's descendant cleanup before returning. A cgroup or platform job object is preferable where available. Add a test in which the precheck calls `setsid`, then assert that the detached child is gone after timeout and station shutdown.

## Verified controls

The following controls were traced to implementation and exercised by the passing test suite:

- **Network identity:** D13 address checks reject public and unspecified bind/dial targets. Link peers use pinned Ed25519 identities in mutual TLS; roles, nonces, transcript signatures, epochs, feature negotiation, revocation, timeouts, frame sizes, and stream credit are checked.
- **Board authorization:** session cookies and one-time links are random and stored only as hashes; passwords use Argon2id; cookie mutations require CSRF; WebSocket upgrades validate Origin; bearer tokens have explicit scopes and expiry; owner-only operations and member share boundaries are enforced server-side and audited.
- **Local IPC:** Unix sockets are created with restrictive permissions, peer UIDs are checked, messages are size-bounded, and malformed secret-bearing input is not echoed.
- **Coordinator ingress:** the MCP endpoint binds only to loopback, requires an exact Host and absent Origin, verifies a per-binding bearer credential, and applies request-size, header/body timeout, rate, connection, and concurrency limits.
- **Secret and process handling:** provider and integration processes use cleared or explicit environments, token-bearing values use redacting wrappers, integration errors scrub known secret forms, and secrets are not placed in command-line arguments where the implementation can avoid it.
- **Filesystem mediation:** canonical containment and no-follow opens defend against traversal and symlink swaps; direct reads, searches, and artifact staging use inode/link checks; member diffs are filtered by allowed paths; output and search sizes are bounded. FDV2-003 is the tracked-Git exception.
- **Browser rendering:** React escaping is preserved, no production `dangerouslySetInnerHTML`, `innerHTML`, `eval`, or `new Function` sink was found, external links use safe opener handling, and served artifacts use a sandbox CSP without same-origin privilege.
- **Persistence and commands:** reviewed SQL uses parameter binding for untrusted values; state-changing commands use stored decisions/receipts and recovery fencing; link methods check feature and scope tables; replay, duplicate, and restart cases have extensive tests.
- **Rust memory safety:** workspace lint policy sets `unsafe_code = "forbid"`.

## Validation results

| Check | Result |
| --- | --- |
| `cargo test --workspace --locked` | Pass; full workspace, integration, transport, TLS, and doc tests |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | Pass |
| `cargo deny check bans licenses sources` | Pass |
| `cargo deny check advisories` using the cached RustSec database with fetch disabled | Pass |
| `cd web && bun run typecheck` | Pass |
| `cd web && bun run lint` | Pass with 24 existing Biome warnings and no errors |
| `cd web && bun test src` | Pass: 1,023 tests across 84 files |
| `cd web && bun audit` | Inconclusive: registry request failed with `DNSResolveFailed`; Bun returned exit code 0 despite the error |

The first sandboxed Rust test attempt could not create a Unix socket and failed with `Operation not permitted`. The same locked workspace suite was rerun outside that sandbox and passed. This was an execution-environment limitation, not a repository test failure.

## Remediation order

1. Fix FDV2-001 and FDV2-002 together so unauthenticated board work has explicit connection, time, and CPU budgets.
2. Fix FDV2-003 before treating private-path mediation as complete for untrusted provider-written workspaces.
3. Make tracker redirect handling explicit and destination-aware for FDV2-004.
4. Reuse setup-command descendant cleanup for scheduled prechecks in FDV2-005.
5. Run `bun audit` in CI or another environment with registry access and record the resolved dependency result.

## Review limitations and residual risk

- No live Jira, Linear, GitHub, GitLab, Claude, or Codex account was used. Their adapters were reviewed through source, fixtures, and fake-process tests.
- No internet-facing penetration test, fuzzing campaign, long-duration resource test, or browser E2E suite was performed for this audit.
- The JavaScript advisory service was unreachable, so this report has no current npm-registry vulnerability result. Rust advisories were checked from the locally cached RustSec database.
- Private credential directories are rescanned every minute. The implementation documents a race for a credential created and rotated entirely inside one interval. Conservative link-count checks protect direct file operations; FDV2-003 remains the material Git snapshot path affected by this class.
- Security still depends on OS account isolation, correct file ownership and permissions, a trustworthy TLS root store, and timely updates to the external `curl`, Git, provider CLI, and browser runtimes.
