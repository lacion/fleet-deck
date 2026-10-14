# Fleet Deck security audit — incomplete standard run

> **Status: incomplete.** Candidate validation did not run. The post-wave critic found six additional coverage units, the follow-up hunting wave and mandatory final-clean critic did not run, and the six candidate fingerprints below remain unvalidated. They are not findings and have no severity.

- Audit date: 2026-10-14
- Source: `46c8ae6d2a9fae9832b8da3ce19c059d34e4dd3a`
- Worktree: tracked source clean; untracked audit tooling excluded
- Profile/scope: standard, all tracked repository source
- Execution: source-only. No target code ran because this host cannot enforce a cumulative scratch-disk limit or usable loopback inside its isolated network namespace.
- Prior evidence: no compatible prior coverage ledger or structured findings; the old prose report was context only.
- Agent invocations: 4 reconnaissance, 3 hunters, 1 post-wave critic; 0 candidate verifiers and 0 final-record verifiers.

## Security posture

Fleet Deck has unusually strong source-visible controls: fail-closed route and method tables, principal/resource rechecks, CSRF and Origin checks, pinned mutual TLS, bounded link admission and multiplexing, same-UID socket checks, no-follow/private-inode filesystem mediation, transactional receipts, scoped coordinator bindings, and careful secret redaction. This run confirmed none of the leads because the required runtime evidence and independent validation were not completed.

## Confirmed findings

None. This is not a clean-audit claim.

## Unvalidated candidate queue

| Fingerprint | Lead | Source trace | Exact blocker / next step |
| --- | --- | --- | --- |
| `browser.local-notes.account-switch-import` | Legacy local review notes may cross browser accounts | `web/src/chat/notes.ts:17` → `web/src/runtime/RuntimeApp.tsx:431` → `web/src/review/importNotes.ts:60` | Run a dummy owner→collaborator account-switch browser fixture and observe whether import exposes the prior note. |
| `local-ipc-same-uid-provider-owner-authority` | Provider children may reach owner-authority local sockets | `crates/fleetdeck-providers/src/host/direct.rs:50` → `crates/fleetdeck-core/src/localsock.rs:63` → `crates/fleetdeck-station/src/local.rs:439` | In a complete sandbox, launch a harmless fake provider and attempt only `SettingsGet` or `Sessions` through the derived socket. |
| `provider-stdout-unbounded-line` | Provider stdout framing has no pre-allocation line cap | `crates/fleetdeck-providers/src/claude/driver.rs:458`, `codex/driver.rs:470` | Under low memory/time limits, emit one line just above a proposed cap and observe whether the session terminates without affecting the station. |
| `provider-stdin-unbounded-response-queue` | Provider-driven automatic replies use an unbounded stdin queue | `crates/fleetdeck-providers/src/claude/requests.rs:144` → `claude/driver.rs:394` → `claude/driver.rs:444` | Use a fake provider that does not read stdin and emits a small fixed request sequence; measure retained queue behavior without stressing the service. |
| `workspace.restore.path-check-use-race` | Restore validates ancestors separately from pathname mutation | `crates/fleetdeck-station/src/workspace/restore.rs:197,250,295,312,423` | In a disposable worktree, swap a checked ancestor for a symlink at a test barrier and verify that a dummy outside marker is untouched. |
| `integrations.asset.auth-origin-port-confusion` | Asset credentials are selected by hostname while curl retains the explicit port | `projects_routes.rs:525` → `integrations/util.rs:205` → `asset.rs:61` → `http.rs:60` | With fake curl and a dummy token, inspect whether `https://tracker.test:8443/` is emitted with Authorization; separately inspect supported deployments for differently controlled secondary ports. |

## Coverage gaps

The validated ledger contains 17 units: 7 covered, 4 candidate, 0 blocked, and 6 deferred. The post-wave critic added these deferred follow-up units:

1. Station command intake, decision, effects, retry, resume, hibernation, and recovery.
2. Shell/PTTY and bidirectional data-stream lifecycle.
3. CLI/TOML/settings/environment/secret lifecycle.
4. HTTP admission, body parsing, WebSocket setup, and connection accounting.
5. Browser login, reconnect, elevation, joining, and account-switch lifecycle.
6. Connected accounts, OAuth, tracker/forge requests, redirects, and credential selection.

The critic also requested blanket reassignment of nearly all completed units without unit-specific reasons; that portion was rejected. The required distinct final-clean critic did not run, so this report makes no clean-coverage claim.

## Hardening notes

- Consider principal-namespacing or clearing legacy browser notes on logout.
- Replace unbounded provider stdout framing and stdin reply queues with explicit byte/cardinality limits and typed session termination.
- Bind privileged local IPC to a per-client capability or isolate provider children from node sockets; UID alone does not distinguish same-user children.
- Use descriptor-relative no-follow operations for restore mutations.
- Compare normalized HTTPS origins, including effective port, before attaching integration credentials.
- Pin container bases by digest and add a macOS CI lane; cap path-based artifact staging.

## Positive source patterns

The review found strong fail-closed method registration, short-lived/single-use capabilities, current-grant checks at decision points, transactional command receipts, bounded link queues/windows/admission, strict Tailscale parsing and generation checks, no production unsafe Rust/FFI path, and sandboxed artifact rendering without same-origin privilege.
