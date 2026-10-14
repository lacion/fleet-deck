# Fleet Deck security audit

> **Completed standard, repository-wide source audit.** No vulnerability was confirmed under the permitted evidence boundary. The audit retains 34 specific `needs_validation` leads and one rejected hypothesis. A needs-validation lead is not a finding, has no severity, and must not be represented as an exploitable vulnerability until its stated blocker is resolved.

- **Audit date:** 2026-10-14
- **Reviewed source:** `4d289c28fbbac7e40a6b5b2d72c49645e34b7817`
- **Profile and scope:** standard; all tracked repository source
- **Budget:** none set
- **Execution boundary:** source-only. Target-controlled builds, tests and fixtures did not run because this host could not provide both an enforceable cumulative scratch-disk quota and usable loopback inside its isolated network namespace. No deployed endpoint, external service, real account or production identity was probed.
- **Prior-run use:** no compatible prior structured ledger or findings existed. The older prose report was regression context only; no result was carried forward.
- **Worktree note:** the reviewed production source was unchanged during continuation; the audit report itself was the only tracked documentation change in scope.
- **Delegated review:** 4 reconnaissance calls, 32 coverage-led hunter calls across 14 waves, 14 immediate post-wave critics, one distinct final-clean critic, 3 Phase 3 verifier batches and 3 separate Phase 5 fresh-eyes batches (57 calls total).
- **Coverage closure:** both the post-Wave-14 critic and a distinct final-clean critic returned clean. There are no deferred, blocked or out-of-scope ledger units.

## Executive assessment

Fleet Deck has a strong source-visible security baseline for a system that centralizes browser, station, provider, workspace and integration authority. The audit found substantial fail-closed authorization, pinned peer identity, scoped link methods, lifecycle rechecks, filesystem containment and bounded protocol handling. It also found 34 concrete paths where a real security boundary may fail, but the decisive result depends on runtime behavior, deployment topology, provider semantics, operating-system races or bounded resource measurements that source alone cannot establish.

The correct result is therefore neither “clean” nor “34 vulnerabilities.” It is: **zero confirmed vulnerabilities, 34 independently source-verified leads requiring controlled validation, and one independently rejected hypothesis.** Owners should resolve the validation queue in a disposable, fully sandboxed environment or by safe configuration observation, starting with authority-confusion, credential-routing, revocation, path-race and shared-service availability leads.

## Results at a glance

| Disposition | Count | Meaning |
| --- | ---: | --- |
| Confirmed | 0 | Complete boundary path and bounded observed result established. |
| Needs validation | 34 | Source-grounded path remains plausible; a decisive runtime or deployment fact is unavailable. |
| Rejected | 1 | Independent source review refuted the claimed distinct security boundary or result. |

## Confirmed findings

None. This does not mean the target has no vulnerabilities; it means no candidate met the required evidence bar under the source-only execution boundary.

## Needs validation

| ID | Lead | Boundary | Source trace | Exact unresolved blocker |
| --- | --- | --- | --- | --- |
| NV-01 | Path artifact publication lacks aggregate and retention bounds<br>`artifacts.path-publication-unbounded-retention-buffering` | artifact_routes::publish → DeckStore::put_artifact_file | `crates/fleetdeck-deck/src/http/artifact_routes.rs:395` → `crates/fleetdeck-station/src/artifacts/service.rs:223` → `crates/fleetdeck-station/src/artifacts/stage.rs:76` → `crates/fleetdeck-station/src/store/artifacts.rs:397` → `crates/fleetdeck-deck/src/http/artifact_routes.rs:164` → `crates/fleetdeck-deck/src/store/artifacts.rs:395` | Target execution is prohibited because the host cannot enforce every required sandbox control, including a cumulative scratch-disk quota and usable isolated loopback; no bounded publication/cache fixture was run. |
| NV-02 | Provider artifact suggestions retain unbounded durable path cardinality<br>`artifacts.provider-suggestion-unbounded-durable-cardinality` | change_paths → Store::artifact_suggest | `crates/fleetdeck-providers/src/codex/map.rs:99` → `crates/fleetdeck-station/src/manager/target.rs:690` → `crates/fleetdeck-station/src/artifacts/suggest.rs:29` → `crates/fleetdeck-station/src/store/artifacts.rs:257` | The required OS sandbox is unavailable, so a bounded disposable fixture could not measure database growth, transaction latency, or pump delay across increasing unique path counts. |
| NV-03 | Codex model-help prompts may inherit host-file read authority<br>`assist.codex-untrusted-prompt-host-read-authority` | assist_routes::generate → assist_routes::generate | `crates/fleetdeck-deck/src/http/assist_routes.rs:338` → `crates/fleetdeck-deck/src/http/assist_routes.rs:254` → `crates/fleetdeck-station/src/assist/prompts.rs:57` → `crates/fleetdeck-station/src/assist/runner.rs:159` → `crates/fleetdeck-station/src/assist/runner.rs:214` → `crates/fleetdeck-deck/src/http/assist_routes.rs:354` | The Codex CLI and sandbox are external dependencies; source alone does not prove which filesystem paths or shell tools the deployed version exposes in read-only mode. |
| NV-04 | Assist CLI output is fully buffered before the nominal byte cap<br>`assist.runner-unbounded-stdout-capture` | intake::assist → CliRunner::run | `crates/fleetdeck-station/src/link/intake.rs:231` → `crates/fleetdeck-station/src/assist/mod.rs:283` → `crates/fleetdeck-station/src/assist/runner.rs:100` → `crates/fleetdeck-station/src/assist/runner.rs:124` | Target execution is prohibited because the complete required OS sandbox, including cumulative disk quota and isolated loopback, is unavailable; no bounded fake-provider memory fixture was run. |
| NV-05 | Browser drafts and legacy notes can cross account transitions<br>`browser.local-notes.account-switch-import` | draftName and writeDraft → Composer state initialization | `web/src/session/Composer.tsx:42` → `web/src/runtime/RuntimeApp.tsx:431` → `web/src/session/SessionView.tsx:452` → `web/src/session/Composer.tsx:138` | The required browser-capable OS sandbox is unavailable, so the account-switch lifecycle and resulting visible dummy text were not observed locally. |
| NV-06 | Terminal output may overwrite the browser clipboard without confirmation<br>`browser.terminal.osc52-unprompted-clipboard-write` | Terminal WebSocket onmessage → OSC 52 handler | `web/src/terminal/Terminal.tsx:193` → `web/src/terminal/Terminal.tsx:197` → `web/src/terminal/osc.ts:11` → `web/src/terminal/Terminal.tsx:243` | Clipboard.writeText authorization differs across supported browser engines, secure-context settings, focused-tab state and previously granted permissions; source alone does not establish that a write from an asynchronous WebSocket/terminal callback succeeds. |
| NV-07 | Bearer API tokens can reach people-only side chat<br>`collaboration.bearer-token-side-chat-access` | API_REST chat routes → DeckStore::chat | `crates/fleetdeck-deck/src/http/methods_table_rest.rs:505` → `crates/fleetdeck-deck/src/http/guard.rs:302` → `crates/fleetdeck-deck/src/http/people_routes.rs:59` → `crates/fleetdeck-deck/src/http/access.rs:64` → `crates/fleetdeck-deck/src/store/chat.rs:69` | Target execution is prohibited because the required isolated-loopback and cumulative-disk sandbox controls are unavailable, so no dummy bearer request was observed. |
| NV-08 | Inline provider credentials may reside in a readable station configuration<br>`config.inline-provider-secret-readable-mode` | RawEnvValue → read_owner_file | `crates/fleetdeck-core/src/config.rs:316` → `crates/fleetdeck-core/src/config.rs:523` → `crates/fleetdeck-core/src/config.rs:698` | Source does not show whether a deployed station.toml contains an inline credential rather than only secret references. |
| NV-09 | Third-party integration error text reaches coordinator MCP output without the untrusted envelope<br>`coordinator.integration-error-untrusted-text-unfenced` | Forge::call → tools/call response | `crates/fleetdeck-station/src/integrations/forge.rs:78` → `crates/fleetdeck-station/src/integrations/exec.rs:49` → `crates/fleetdeck-station/src/integrations/mod.rs:151` → `crates/fleetdeck-deck/src/link/coordinator_reads.rs:53` → `crates/fleetdeck-deck/src/link/requests.rs:88` → `crates/fleetdeck-station/src/coordinator/jsonrpc.rs:100` | This assignment forbids target-code execution, and source alone cannot establish whether a supported coordinator model will treat an attacker-chosen MCP error message as instructions and issue an unrequested mutation within its existing grant. |
| NV-10 | Terminal coordinator outbox rows have no retention bound<br>`coordinator.outbox-terminal-retention-unbounded` | coordinator::ingress::handle → schema V17 coordinator_outbox | `crates/fleetdeck-station/src/coordinator/ingress.rs:204` → `crates/fleetdeck-station/src/coordinator/outbox.rs:152` → `crates/fleetdeck-station/src/store/coordinator_outbox.rs:208` → `crates/fleetdeck-station/src/store/coordinator_outbox.rs:276` → `crates/fleetdeck-station/src/store/schema.rs:413` | Target execution is prohibited because the full required sandbox is unavailable, so no bounded dummy SQLite lifecycle fixture measured retained rows or bytes. |
| NV-11 | Unauthenticated refusals can grow the durable deck audit log without retention<br>`deck.audit.unauthenticated-refusal-unbounded-retention` | guard → DeckStore::audit | `crates/fleetdeck-deck/src/http/guard.rs:415` → `crates/fleetdeck-deck/src/http/guard.rs:434` → `crates/fleetdeck-deck/src/http/auth_routes.rs:181` → `crates/fleetdeck-deck/src/store/audit.rs:107` | The required isolated-loopback, cumulative-disk-limited OS sandbox is unavailable, so request-to-row growth, database/WAL growth, latency, and recovery near a safe storage threshold were not observed. |
| NV-12 | A superseded station link can commit a stale snapshot over newer deck state<br>`event-sync.superseded-stale-snapshot-commit` | control::on_frame → review::delete_unlisted | `crates/fleetdeck-link/src/control.rs:713` → `crates/fleetdeck-link/src/table.rs:139` → `crates/fleetdeck-deck/src/link/mod.rs:198` → `crates/fleetdeck-deck/src/link/mod.rs:372` → `crates/fleetdeck-deck/src/store/read_model.rs:286` → `crates/fleetdeck-deck/src/store/review.rs:341` | Target execution was prohibited because the required OS-enforced sandbox controls were unavailable: empty allowlisted environment, disabled external networking, read-only target and toolchain, scratch-only writes, and explicit CPU, memory, process, file-size, disk, and wall-clock limits. The empty artifact promotion allowlist also precluded retained observed evidence. |
| NV-13 | Global authentication failure windows can deny unrelated sources<br>`http.auth.global-failure-window-lockout` | auth_routes::password → auth_routes::elevate | `crates/fleetdeck-deck/src/http/auth_routes.rs:245` → `crates/fleetdeck-deck/src/http/admission.rs:315` → `crates/fleetdeck-deck/src/http/auth_routes.rs:159` → `crates/fleetdeck-deck/src/http/admission.rs:254` → `crates/fleetdeck-deck/src/http/auth_routes.rs:521` | Target execution is prohibited because the required network-isolated, empty-environment, read-only-target, scratch-only, fully resource-limited sandbox is unavailable. |
| NV-14 | Tracker asset credentials are authorized by hostname rather than HTTPS origin<br>`integrations.asset.auth-origin-port-confusion` | projects_routes::asset → http::config | `crates/fleetdeck-deck/src/http/projects_routes.rs:525` → `crates/fleetdeck-proto/src/integrations/checks.rs:234` → `crates/fleetdeck-station/src/integrations/util.rs:199` → `crates/fleetdeck-station/src/integrations/asset.rs:49` → `crates/fleetdeck-station/src/integrations/http.rs:51` | The required sandbox is unavailable, so a dummy fake-curl fixture could not independently observe the generated URL/header pair. |
| NV-15 | A failed durable revocation is reported as success and can revive after restart<br>`link.revocation-store-failure-peer-revival` | local::pairing → StationNode::start_with_dirs | `crates/fleetdeck-station/src/local.rs:274` → `crates/fleetdeck-station/src/link/runtime.rs:626` → `crates/fleetdeck-link/src/revoke.rs:54` → `crates/fleetdeck-station/src/link/peers.rs:124` → `crates/fleetdeck-station/src/store/peers.rs:186` → `crates/fleetdeck-station/src/link/runtime.rs:238` | Target execution was prohibited, so no bounded test injected a transient SQLite write failure after the persistent key lookup and then restarted a node against the unchanged dummy database. |
| NV-16 | Reissued bearer tokens inherit revoked-token read cursors<br>`metadata.read-cursor-token-name-reuse` | mark_read → DeckStore::sessions_for | `crates/fleetdeck-deck/src/http/activity_routes.rs:27` → `crates/fleetdeck-deck/src/store/read_cursors.rs:21` → `crates/fleetdeck-deck/src/store/auth.rs:107` → `crates/fleetdeck-deck/src/store/read_cursors.rs:108` | The required sandbox is unavailable, so the two-issuance dummy-store sequence and resulting unread count were not independently observed. |
| NV-17 | Queued member mail can outlive share revocation<br>`orchestration.member-mail-stale-share-authority` | send_mail → DeckLink::attempt | `crates/fleetdeck-deck/src/http/fleet_routes.rs:144` → `crates/fleetdeck-deck/src/http/fleet_routes.rs:185` → `crates/fleetdeck-deck/src/store/fleet.rs:178` → `crates/fleetdeck-deck/src/store/shares.rs:208` → `crates/fleetdeck-deck/src/store/outbox.rs:434` → `crates/fleetdeck-deck/src/link/outbox.rs:59` | The assignment is source-only and the host lacks the required cumulative disk quota and usable isolated loopback sandbox controls, so target code could not be executed to observe a dummy queued mail crossing a share revoke/reconnect sequence. |
| NV-18 | Closed provider requests accumulate in durable adapter checkpoints<br>`provider-adapter-checkpoint-unbounded-request-state` | on_control_request → store_checkpoint | `crates/fleetdeck-providers/src/claude/requests.rs:88` → `crates/fleetdeck-providers/src/claude/requests.rs:165` → `crates/fleetdeck-station/src/manager/durable.rs:590` → `crates/fleetdeck-station/src/store/shims.rs:601` | Target execution is prohibited because the complete required sandbox is unavailable, so no bounded sans-I/O request sequence or checkpoint-size measurement was run. |
| NV-19 | Provider-driven replies can accumulate in an unbounded stdin queue<br>`provider-stdin-unbounded-response-queue` | on_control_request → write_loop | `crates/fleetdeck-providers/src/claude/requests.rs:144` → `crates/fleetdeck-providers/src/claude/driver.rs:385` → `crates/fleetdeck-providers/src/claude/driver.rs:424` | The required OS sandbox is unavailable, so a fake provider that withholds stdin reads could not be used to observe bounded queue growth and isolation from another dummy session. |
| NV-20 | Provider stdout accepts unbounded protocol lines in the shared station process<br>`provider-stdout-unbounded-line` | DirectChildHost::spawn → read_loop | `crates/fleetdeck-providers/src/host/direct.rs:57` → `crates/fleetdeck-providers/src/claude/driver.rs:458` → `crates/fleetdeck-providers/src/claude/driver.rs:461` | Target execution was prohibited because the available platform cannot supply the complete required OS sandbox with all mandated network, filesystem, environment, process, resource, disk, and wall-time controls; the artifact promotion allowlist is empty. |
| NV-21 | Duplicate outstanding Codex IDs can make approval correlation ambiguous<br>`provider.codex-duplicate-native-id-approval-confusion` | rpc::parse_value → CodexCore::answer RPC reply | `crates/fleetdeck-providers/src/codex/rpc.rs:60` → `crates/fleetdeck-providers/src/codex/requests.rs:205` → `crates/fleetdeck-providers/src/codex/requests.rs:382` → `crates/fleetdeck-providers/src/codex/requests.rs:445` | Target execution is prohibited because the host lacks the complete required sandbox, so no bounded protocol harness was run. |
| NV-22 | Managed services persist unvalidated executable search paths<br>`service.persisted-path-executable-hijack` | Context::detect → ClaudeAdapter::run_probe | `crates/fleetdeck-cli/src/service/mod.rs:93` → `crates/fleetdeck-cli/src/service/mod.rs:368` → `crates/fleetdeck-cli/src/service/systemd.rs:113` → `crates/fleetdeck-core/src/config.rs:409` → `crates/fleetdeck-station/src/link/runtime.rs:285` → `crates/fleetdeck-providers/src/claude/driver.rs:328` | Source does not reveal the effective PATH stored by a deployed service or the owner, mode, and ACL of each component; a lower-trust writable component is required. |
| NV-23 | Provider-reported Codex session IDs can be reinterpreted as TUI options<br>`shell.handover-native-id-argv-option-confusion` | append_one session.started projection → Shells::spawn | `crates/fleetdeck-station/src/store/log.rs:166` → `crates/fleetdeck-station/src/store/terminal.rs:80` → `crates/fleetdeck-station/src/shell/handover.rs:103` → `crates/fleetdeck-station/src/shell/mod.rs:525` | This run is source-only because the host cannot provide the required cumulative disk quota and isolated loopback sandbox controls, so the provider TUI parser was not executed. |
| NV-24 | Terminal handover does not revalidate current workspace or bypass-mode policy<br>`shell.handover-stale-launch-policy` | handover::admit → Shells::spawn | `crates/fleetdeck-station/src/shell/handover.rs:148` → `crates/fleetdeck-station/src/shell/dispatch.rs:82` → `crates/fleetdeck-station/src/shell/dispatch.rs:148` → `crates/fleetdeck-station/src/shell/handover.rs:87` → `crates/fleetdeck-station/src/shell/mod.rs:525` | Target execution is prohibited because the required sandbox controls, including cumulative disk quota, are unavailable; no fake TUI process was started. |
| NV-25 | Drain can report idle before a prechecked command commits new work<br>`station.drain-prechecked-submit-after-idle` | member_command → recover_effects | `crates/fleetdeck-deck/src/http/command_routes.rs:290` → `crates/fleetdeck-station/src/manager/mod.rs:577` → `crates/fleetdeck-station/src/manager/drain.rs:87` → `crates/fleetdeck-core/src/db.rs:169` → `crates/fleetdeck-station/src/store/effects.rs:753` | The required sandbox is unavailable, so the decisive interleaving and its post-idle receipt/effect state were not reproduced with a deterministic test barrier. |
| NV-26 | Production station never runs its configured event and receipt retention<br>`station.event-receipt-prune-unscheduled` | provider event pump → log::append_at | `crates/fleetdeck-station/src/manager/target.rs:704` → `crates/fleetdeck-station/src/store/mod.rs:390` → `crates/fleetdeck-station/src/store/log.rs:138` | Target execution was prohibited because the required OS-enforced sandbox controls were unavailable: empty allowlisted environment, disabled external networking, read-only target and toolchain, scratch-only writes, and explicit CPU, memory, process, file-size, disk, and wall-clock limits. The empty artifact promotion allowlist also precluded retained observed evidence. |
| NV-27 | Hibernated input can become an uncounted resume during restart<br>`station.restart-hibernated-input-resume-bypass` | command_routes::member_command → recover_effects | `crates/fleetdeck-deck/src/http/command_routes.rs:294` → `crates/fleetdeck-station/src/manager/restart_gate.rs:165` → `crates/fleetdeck-station/src/manager/decide.rs:146` → `crates/fleetdeck-station/src/manager/decide.rs:704` → `crates/fleetdeck-station/src/settings_svc.rs:283` → `crates/fleetdeck-station/src/store/effects.rs:753` | Target execution is prohibited because the complete required sandbox is unavailable, so the cross-gate schedule and accepted-then-retired receipt were not observed. |
| NV-28 | Terminal station recovery records have no retention lifecycle<br>`station.terminal-recovery-state-unbounded-retention` | accepted session command → log::prune | `crates/fleetdeck-station/src/manager/decide.rs:581` → `crates/fleetdeck-station/src/store/effects.rs:326` → `crates/fleetdeck-station/src/store/effects.rs:544` → `crates/fleetdeck-station/src/store/log.rs:295` | The required sandbox is unavailable, so bounded repeated lifecycle cycles could not establish retained row/byte growth after recovery and pruning. |
| NV-29 | Failed live-upgrade marker can let rollback restore revoked station authority<br>`station.upgrade-live-marker-fail-open-rollback` | RunningStation::start → StationNode startup peer load | `crates/fleetdeck-station/src/serve.rs:152` → `crates/fleetdeck-station/src/upgrade.rs:131` → `crates/fleetdeck-station/src/serve.rs:165` → `crates/fleetdeck-station/src/store/peers.rs:186` → `crates/fleetdeck-cli/src/upgrade.rs:146` → `crates/fleetdeck-station/src/upgrade.rs:267` → `crates/fleetdeck-station/src/link/runtime.rs:238` | Target execution was prohibited because the required OS-enforced sandbox controls were unavailable: empty allowlisted environment, disabled external networking, read-only target and toolchain, scratch-only writes, and explicit CPU, memory, process, file-size, disk, and wall-clock limits. The empty artifact promotion allowlist also precluded retained observed evidence. |
| NV-30 | Workspace preflight buffers complete Git remote output<br>`workspace.preflight.unbounded-git-output` | workspace_routes::preflight → Git::status | `crates/fleetdeck-deck/src/http/workspace_routes.rs:118` → `crates/fleetdeck-station/src/link/intake.rs:145` → `crates/fleetdeck-station/src/workspace/preflight.rs:49` → `crates/fleetdeck-station/src/workspace/git.rs:231` | Target execution is prohibited because the host lacks cumulative disk quota and usable isolated loopback as part of the required sandbox, so no fake-Git memory fixture ran. |
| NV-31 | Missing HOME can omit provider credential files from workspace privacy policy<br>`workspace.private-path-home-unset-disclosure` | file_routes::read → files::read_for | `crates/fleetdeck-deck/src/http/file_routes.rs:249` → `crates/fleetdeck-station/src/serve.rs:187` → `crates/fleetdeck-station/src/workspace/private_paths.rs:154` → `crates/fleetdeck-station/src/manager/files.rs:49` → `crates/fleetdeck-station/src/workspace/files.rs:356` | Source does not reveal whether a station runs without an absolute HOME while an explicit allowed root contains its actual user home. |
| NV-32 | Checkpoint restore can follow an ancestor swapped after validation<br>`workspace.restore.path-check-use-race` | Worktree::plan_restore → Worktree::apply_restore | `crates/fleetdeck-station/src/workspace/restore.rs:197` → `crates/fleetdeck-station/src/workspace/restore.rs:250` → `crates/fleetdeck-station/src/workspace/restore.rs:423` → `crates/fleetdeck-station/src/manager/revert_run.rs:264` → `crates/fleetdeck-station/src/workspace/restore.rs:295` | Target execution is prohibited because the required empty-environment, network-isolated, read-only-target, scratch-only, resource-limited OS sandbox is unavailable. |
| NV-33 | Review anchor cache can outlive a stricter member file policy<br>`workspace.review-anchor.member-deny-cache-stale` | review_routes::add → review_routes::add response | `crates/fleetdeck-deck/src/http/review_routes.rs:313` → `crates/fleetdeck-deck/src/http/review_anchor_cache.rs:82` → `crates/fleetdeck-deck/src/http/settings_routes.rs:493` → `crates/fleetdeck-station/src/settings_svc.rs:149` → `crates/fleetdeck-deck/src/http/review_routes.rs:271` → `crates/fleetdeck-deck/src/store/review.rs:658` → `crates/fleetdeck-deck/src/http/review_routes.rs:365` | Target execution is prohibited because the complete required sandbox is unavailable, so no fake-clock dummy-member fixture observed the stale protected anchor disclosure. |
| NV-34 | Read-scoped session diffs can repeatedly materialize large workspace snapshots<br>`workspace.session-diff-unthrottled-materialization` | file_routes::session_diff → Worktree::stage_changes_with | `crates/fleetdeck-deck/src/http/file_routes.rs:334` → `crates/fleetdeck-deck/src/http/methods_table.rs:314` → `crates/fleetdeck-station/src/link/mod.rs:141` → `crates/fleetdeck-station/src/workspace/session_diff.rs:148` → `crates/fleetdeck-station/src/workspace/checkpoint.rs:401` → `crates/fleetdeck-station/src/workspace/stage.rs:339` | The required sandbox is unavailable, so CPU, RSS, Git child concurrency, cancellation, and object-database growth were not measured with bounded synthetic worktrees. |

## Detailed needs-validation records

### NV-01: Path artifact publication lacks aggregate and retention bounds

- **Fingerprint:** `artifacts.path-publication-unbounded-retention-buffering`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** A Drive-scoped principal can publish an eligible existing file. The station copies path publications to durable version directories without a per-file, bundle, aggregate-byte, artifact-count, or retained-version limit; HTML publication also reads the complete main file before discovering referenced files. When the deck fills its cache, it reconstructs each file in one Vec and writes the complete value to SQLite. The source path is real, but a safe bounded fixture and deployment capacity facts are required to establish meaningful shared availability impact.
- **Claimed root cause:** Path publication and artifact retention do not share a cumulative byte or cardinality quota, and deck cache fill reconstructs each file in memory before storing it as one SQLite value.

Source trace:

1. **entrypoint — artifact_routes::publish:** `crates/fleetdeck-deck/src/http/artifact_routes.rs:395` — A Drive-authorized caller supplies a station and eligible artifact path.
2. **propagation — ArtifactService::publish_owned:** `crates/fleetdeck-station/src/artifacts/service.rs:223` — The station resolves the path and stages it without a byte or file-count budget.
3. **propagation — copy_hashed:** `crates/fleetdeck-station/src/artifacts/stage.rs:76` — The source is copied in 64 KiB chunks until EOF with no total-size check.
4. **propagation — store::artifacts::commit:** `crates/fleetdeck-station/src/store/artifacts.rs:397` — Distinct manifests create another version row and the staged tree is moved into permanent artifact storage.
5. **propagation — fill_cache:** `crates/fleetdeck-deck/src/http/artifact_routes.rs:164` — The deck appends every bounded wire chunk to one Vec until EOF.
6. **sink — DeckStore::put_artifact_file:** `crates/fleetdeck-deck/src/store/artifacts.rs:395` — The complete reconstructed bytes are stored as one artifact_cache SQLite value without source-visible eviction or quota.

Verified source evidence:

- `crates/fleetdeck-deck/src/http/methods_table.rs:272` — The publication route requires Drive scope, identifying an authenticated but lower-trust caller.
- `crates/fleetdeck-station/src/artifacts/stage.rs:8` — The module explicitly states that streamed path publication has no size cap.
- `crates/fleetdeck-station/src/artifacts/stage.rs:159` — HTML staging reads the complete staged main file before reference parsing.
- `crates/fleetdeck-station/src/artifacts/stage.rs:203` — Inline content has an 8 MiB cap that the path flow does not reuse.
- `crates/fleetdeck-proto/src/artifacts.rs:29` — The protocol caps one response chunk at 700 KiB but not a complete file.

Why it remains unconfirmed:

- Target execution is prohibited because the host cannot enforce every required sandbox control, including a cumulative scratch-disk quota and usable isolated loopback; no bounded publication/cache fixture was run.
- Deployment filesystem quotas, deck and station memory ceilings, SQLite limits, cleanup policy, and supervisor blast radius are not source-visible.

Safe validation plan:

- **Local:** In an approved no-network sandbox with a read-only target, scratch-only writes, and strict memory, file, cumulative-disk, process and time limits, publish small synthetic files at increasing bounded sizes and a fixed number of changing versions, then request one cache fill. Record only station artifact bytes, deck database bytes and peak RSS, stopping at the first linear growth point.
- **Owner-observed deployment check:** The owner should inventory Drive principals and read-only inspect station/deck storage quotas, artifact retention or cleanup jobs, cgroup memory ceilings, SQLite growth monitoring and supervisor restart scope; do not generate production load.

### NV-02: Provider artifact suggestions retain unbounded durable path cardinality

- **Fingerprint:** `artifacts.provider-suggestion-unbounded-durable-cardinality`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** A lower-trust provider can report arbitrarily many distinct HTML path strings. Fleet Deck accepts each fresh session/path pair as a durable suggestion and appends an event, while the suggestion table has no source-visible quota or retention lifecycle. Per-batch and exact-duplicate limits do not bound aggregate station database growth or serial insert work.
- **Claimed root cause:** Artifact suggestion admission bounds neither path length nor aggregate unique paths, and artifact_suggestions has only a session/path uniqueness key with no count, byte, age, or deletion policy.

Source trace:

1. **entrypoint — change_paths:** `crates/fleetdeck-providers/src/codex/map.rs:99` — Provider-controlled fileChange paths are collected without a count or semantic path bound.
2. **propagation — pump:** `crates/fleetdeck-station/src/manager/target.rs:690` — The station stores bounded batches but repeatedly passes every stored provider event to artifact suggestion observation.
3. **propagation — paths and observe:** `crates/fleetdeck-station/src/artifacts/suggest.rs:29` — HTML suffix and hidden-directory checks admit non-existent paths, deduplicate only the current call, and submit every accepted path sequentially.
4. **sink — Store::artifact_suggest:** `crates/fleetdeck-station/src/store/artifacts.rs:257` — Each fresh session/path pair is inserted permanently and creates an ArtifactSuggested event in its own transaction.

Verified source evidence:

- `crates/fleetdeck-station/src/store/schema.rs:526` — artifact_suggestions has a composite primary key but no retention, quota, foreign-key cascade, or size metadata.
- `crates/fleetdeck-station/src/store/log.rs:295` — The general pruner removes eligible events and old command receipts, not artifact_suggestions.
- `crates/fleetdeck-station/src/manager/target.rs:38` — The 64-event pump batch and 256-event channel bound transient Event objects but not lifetime unique paths.

Why it remains unconfirmed:

- The required OS sandbox is unavailable, so a bounded disposable fixture could not measure database growth, transaction latency, or pump delay across increasing unique path counts.
- Deployment disk quotas, SQLite limits, supervisor isolation, practical provider rates, and the availability threshold are not source-visible.

Safe validation plan:

- **Local:** In the required offline sandbox with a scratch-only SQLite store and strict CPU, memory, disk, process, output, and wall limits, feed modest synthetic fileChange batches containing unique short .html paths. Record row count, database/WAL bytes, emitted suggestions, and pump latency at small increasing counts; verify exact repeats add no rows and a per-session/aggregate quota produces a bounded refusal.
- **Owner-observed deployment check:** The owner should inspect existing artifact_suggestions cardinality, station database growth alerts, filesystem quotas, and session-retention policy without generating new suggestions or stressing a live station.

### NV-03: Codex model-help prompts may inherit host-file read authority

- **Fingerprint:** `assist.codex-untrusted-prompt-host-read-authority`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** A Drive-scoped bearer can submit issue text for summarization. Fleet Deck fences that text and tells the model not to obey it, but those prompts are only guardrails. The Codex assist process is launched with a read-only sandbox rather than with tools disabled, while its reconstructed environment retains HOME and other owner context. If the deployed Codex CLI permits its normal shell tools to read outside the private empty cwd, attacker-controlled prompt content can cause reads of owner files that the bearer cannot obtain through Fleet Deck's path-filtered file API and return them as generated text. Claude has an explicit empty tool set, so the candidate is specific to the Codex branch.
- **Claimed root cause:** The Codex model-help subprocess receives model-controlled tool authority under the station user's read access; a private cwd and read-only write policy do not deterministically constrain reads to an allowlisted directory, and prompt fencing is treated as the only action-binding control.

Source trace:

1. **entrypoint — assist_routes::generate:** `crates/fleetdeck-deck/src/http/assist_routes.rs:338` — An authenticated Drive principal submits a model-help request; IssueDigest accepts caller-supplied title and body and routes the generated prompt to a station.
2. **propagation — build:** `crates/fleetdeck-deck/src/http/assist_routes.rs:254` — Issue title and body are truncated and fenced as untrusted text, but remain attacker-controlled model input.
3. **propagation — prompts::build:** `crates/fleetdeck-station/src/assist/prompts.rs:57` — The content is embedded in a fixed prompt with a natural-language instruction not to follow instructions inside it.
4. **propagation — codex_command:** `crates/fleetdeck-station/src/assist/runner.rs:159` — The station starts codex exec with --sandbox read-only but no equivalent of Claude's --tools empty restriction.
5. **propagation — parse_codex:** `crates/fleetdeck-station/src/assist/runner.rs:214` — The last Codex agent_message is accepted as the generated answer.
6. **sink — assist_routes::generate:** `crates/fleetdeck-deck/src/http/assist_routes.rs:354` — The station answer is returned to the requesting bearer as generated text.

Verified source evidence:

- `crates/fleetdeck-deck/src/http/methods_table.rs:203` — The route deliberately authorizes Drive principals because model help spends owner tokens, but it does not grant arbitrary host-file access.
- `crates/fleetdeck-core/src/childenv.rs:3` — The child environment intentionally retains the owner's normal HOME, Git and SSH settings while removing provider-routing variables.
- `crates/fleetdeck-core/src/childenv.rs:67` — The base environment is filtered rather than replaced with a minimal filesystem-neutral environment.
- `crates/fleetdeck-station/src/assist/runner.rs:80` — The strongest visible controls are an empty private cwd, env_clear followed by the reconstructed instance environment, piped I/O, and kill-on-drop.
- `crates/fleetdeck-station/src/assist/runner.rs:142` — Claude explicitly receives --tools with an empty value, demonstrating the intended no-tool capability boundary.
- `crates/fleetdeck-station/src/assist/runner.rs:157` — The Codex branch supplies only a read-only sandbox and does not disable model tools or constrain readable roots.

Why it remains unconfirmed:

- The Codex CLI and sandbox are external dependencies; source alone does not prove which filesystem paths or shell tools the deployed version exposes in read-only mode.
- Source-only review cannot demonstrate that a model follows injected content and returns a sentinel from outside the private cwd.
- Deployment may select only Claude, so actual Codex reachability is configuration-dependent.

Safe validation plan:

- **Local:** In a disposable VM or equivalent enforced sandbox with no real credentials, configure a temporary HOME containing a unique harmless sentinel outside the assist cwd, use the supported Codex CLI version and a dummy or mocked provider, submit a bounded IssueDigest prompt requesting that sentinel, and record tool events plus the returned text. Repeat with an allowlisted-cwd/no-tools fix to establish prevention.
- **Owner-observed deployment check:** The owner should verify whether any station currently selects a Codex provider for model help and record the exact Codex CLI version and effective read-only sandbox policy; do not test against production data.

### NV-04: Assist CLI output is fully buffered before the nominal byte cap

- **Fingerprint:** `assist.runner-unbounded-stdout-capture`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** The assist runner starts a provider CLI with piped stdout and a 60-second timeout, but wait_with_output first accumulates the complete stream. Only after process completion does the code slice the Vec to 256 KiB. Two concurrent-call slots bound process count, not bytes, so a faulty or adversarial provider output stream can transfer unbounded memory pressure into the shared station process during the deadline.
- **Claimed root cause:** The 256 KiB output limit is applied after tokio::process::Child::wait_with_output has already collected all stdout rather than at the pipe reader.

Source trace:

1. **entrypoint — intake::assist:** `crates/fleetdeck-station/src/link/intake.rs:231` — An authorized assist request selects a configured provider runner.
2. **propagation — Assist::text:** `crates/fleetdeck-station/src/assist/mod.rs:283` — A semaphore limits simultaneous calls to two before invoking the CLI runner.
3. **propagation — CliRunner::run:** `crates/fleetdeck-station/src/assist/runner.rs:100` — The timeout encloses child.wait_with_output, which returns only after collecting stdout.
4. **sink — CliRunner::run:** `crates/fleetdeck-station/src/assist/runner.rs:124` — The code truncates only the already-complete output Vec, after the allocation occurred.

Verified source evidence:

- `crates/fleetdeck-station/src/assist/runner.rs:20` — A 60-second deadline limits duration but not bytes emitted during that interval.
- `crates/fleetdeck-station/src/assist/runner.rs:22` — MAX_OUTPUT_BYTES is 256 KiB.
- `crates/fleetdeck-station/src/assist/runner.rs:85` — Provider stdout is piped into the station process.
- `crates/fleetdeck-station/src/assist/mod.rs:36` — At most two assist calls execute concurrently.

Why it remains unconfirmed:

- Target execution is prohibited because the complete required OS sandbox, including cumulative disk quota and isolated loopback, is unavailable; no bounded fake-provider memory fixture was run.
- Station memory limits and supervisor/restart blast radius are deployment facts.

Safe validation plan:

- **Local:** In an approved sandbox with strict memory and time limits, point CliRunner at a deterministic fixture executable that emits several small increasing stdout sizes. Measure peak RSS and verify it grows past the declared 256 KiB before parsing; retain no emitted payload. A fixed regression must stream through a capped reader and terminate the child on overflow.
- **Owner-observed deployment check:** The owner should document station cgroup/container memory limits and whether a station restart interrupts unrelated active sessions; no production saturation is needed.

### NV-05: Browser drafts and legacy notes can cross account transitions

- **Fingerprint:** `browser.local-notes.account-switch-import`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** Composer drafts and legacy review notes are keyed only by station and session. Logout clears login/member state but not those browser records. A later owner or collaborator who can open the same session can therefore see the prior principal's draft, and can explicitly import the prior note text as a draft comment attributed to the current principal.
- **Claimed root cause:** Browser authoring state omits principal or login-session identity from its storage namespace and is neither cleared nor rotated during logout and account transition.

Source trace:

1. **entrypoint — draftName and writeDraft:** `web/src/session/Composer.tsx:42` — Unsent prompt text is stored in sessionStorage under fd-draft:<station>/<session> without principal identity.
2. **propagation — chrome.onLogout:** `web/src/runtime/RuntimeApp.tsx:431` — Logout clears the password and member fallback but does not remove draft or legacy-note keys.
3. **propagation — Composer mount:** `web/src/session/SessionView.tsx:452` — A later principal with composer access mounts Composer with a key derived only from the same station and session IDs.
4. **sink — Composer state initialization:** `web/src/session/Composer.tsx:138` — The stale draft is loaded into and displayed by the current principal's composer, whose send path uses the current account's actions.

Verified source evidence:

- `web/src/runtime/connection.ts:109` — Connection logout resets server authentication and transport state without browser authoring-storage cleanup.
- `web/src/chat/notes.ts:18` — Legacy notes use origin-wide localStorage keyed only by station and session.
- `web/src/review/ImportPrompt.tsx:27` — The current review writer is offered notes loaded solely through that principal-free key.
- `web/src/review/importNotes.ts:60` — Explicit import submits the stale body through the current principal's review action.
- `crates/fleetdeck-deck/src/store/review.rs:651` — The server correctly attributes the imported draft to the currently authenticated viewer and cannot identify the browser principal that created the stale note.

Why it remains unconfirmed:

- The required browser-capable OS sandbox is unavailable, so the account-switch lifecycle and resulting visible dummy text were not observed locally.

Safe validation plan:

- **Local:** In an approved isolated browser fixture with dummy principals, authenticate as A, type a unique unsent composer draft and seed one legacy fd-notes record, log out in the same tab, authenticate as B with access to that session, and verify B sees A's draft. Then open Changes, import the note once, and confirm the draft comment is attributed to B. Clear both keys and stop.

### NV-06: Terminal output may overwrite the browser clipboard without confirmation

- **Fingerprint:** `browser.terminal.osc52-unprompted-clipboard-write`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** A process whose bytes are displayed in an open Fleet Deck terminal can emit an OSC 52 control sequence containing attacker-chosen text. The browser client parses that sequence and immediately attempts to replace the user's clipboard without showing the value or requiring an explicit copy action. The parser blocks clipboard reads, malformed encodings, invalid UTF-8, empty values and values over 100,000 characters, but those are confidentiality and size controls rather than an authorization decision for clipboard writes. If the supported browser permits the write in this context, a compromised workload, repository command, remote host or model-driven terminal program can silently stage text that the user later pastes into another shell, prompt or application. The source does not automatically execute the staged text, so the claimed boundary effect is limited to clipboard integrity and the ensuing user-mediated paste.
- **Claimed root cause:** OSC 52 is treated as trusted UI intent: station-controlled terminal bytes are connected directly to navigator.clipboard.writeText without a user gesture, confirmation, per-session opt-in, or default-deny policy for clipboard writes.

Source trace:

1. **entrypoint — Terminal WebSocket onmessage:** `web/src/terminal/Terminal.tsx:193` — Bytes produced by the station shell or TUI arrive as WebSocket text, ArrayBuffer or Blob messages while the terminal is open.
2. **propagation — Terminal WebSocket onmessage:** `web/src/terminal/Terminal.tsx:197` — All accepted message forms are passed directly to xterm.write, which interprets terminal control sequences including OSC 52.
3. **propagation — oscCopyText:** `web/src/terminal/osc.ts:11` — The OSC payload is decoded as base64 UTF-8 and accepted when nonempty and no longer than 100,000 characters; the selector is not tied to a user action or trust state.
4. **sink — OSC 52 handler:** `web/src/terminal/Terminal.tsx:243` — The registered handler calls navigator.clipboard.writeText for every accepted payload and silently ignores rejection, without prompting or requiring a copy gesture.

Verified source evidence:

- `web/src/terminal/Terminal.tsx:193` — The terminal display consumes raw station-controlled WebSocket frames rather than a text-only escaped format.
- `web/src/terminal/Terminal.tsx:212` — Normal selection copying is explicitly bound to Ctrl+Shift+C or Cmd+C, showing an available user-generated copy path distinct from OSC output.
- `web/src/terminal/Terminal.tsx:242` — The OSC 52 comment and handler intentionally let the agent copy to the clipboard, and the handler contains no confirmation or activation check.
- `web/src/terminal/osc.ts:15` — The strongest confidentiality control rejects OSC read requests represented by a question mark.
- `web/src/terminal/osc.ts:25` — The strongest availability control rejects decoded clipboard text over 100,000 characters, but accepted values retain arbitrary attacker-chosen content.

Why it remains unconfirmed:

- Clipboard.writeText authorization differs across supported browser engines, secure-context settings, focused-tab state and previously granted permissions; source alone does not establish that a write from an asynchronous WebSocket/terminal callback succeeds.
- The source-only boundary prohibited running target or browser code, and the host lacks the approved OS-enforced browser sandbox controls required for a bounded local observation.

Safe validation plan:

- **Local:** In an approved isolated browser sandbox with a dummy Fleet Deck origin and no real clipboard data, open a dummy terminal, initialize the clipboard to a harmless sentinel, and inject one minimal OSC 52 frame containing a different harmless sentinel through a controlled local WebSocket fixture without any user click or keypress. Under each supported browser's default permissions, record whether the clipboard changes, then repeat after tab blur and with an explicit per-session opt-in/confirmation implementation. Stop after the first observed write or deterministic denial and capture only the two dummy sentinel values and browser version.

### NV-07: Bearer API tokens can reach people-only side chat

- **Fingerprint:** `collaboration.bearer-token-side-chat-access`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** The side-chat module explicitly defines a person as an owner or invited member and rejects bearer tokens in its person helper. The GET and POST chat handlers omit that helper. Their method-table entries accept Read or Drive bearer scopes, and session_of applies covering-share and member-role checks only to member principals, allowing a token that knows an existing station/session to reach chat storage. A bounded request fixture is required before this authorization defect can be confirmed.
- **Claimed root cause:** The chat routes rely on generic Read/Drive scope and session existence but omit the module's person-only check, while session_of deliberately performs share authorization only for Principal::Member.

Source trace:

1. **entrypoint — API_REST chat routes:** `crates/fleetdeck-deck/src/http/methods_table_rest.rs:505` — GET chat accepts Read and POST chat accepts Drive; both are member-capable rather than owner-only.
2. **propagation — guard::check:** `crates/fleetdeck-deck/src/http/guard.rs:302` — A bearer principal is admitted when its global token scope satisfies the route.
3. **propagation — people_routes::chat:** `crates/fleetdeck-deck/src/http/people_routes.rs:59` — The GET handler calls session_of directly and does not call person.
4. **propagation — access::session_of:** `crates/fleetdeck-deck/src/http/access.rs:64` — Share coverage and role enforcement run only for Principal::Member, not Principal::Token.
5. **sink — DeckStore::chat:** `crates/fleetdeck-deck/src/store/chat.rs:69` — The store returns chat rows after the handler's incomplete principal check; the POST sibling similarly inserts a token-authored message.

Verified source evidence:

- `crates/fleetdeck-deck/src/http/people_routes.rs:28` — person explicitly rejects Principal::Token because a bearer token is not a person.
- `crates/fleetdeck-deck/src/http/people_routes.rs:87` — post_chat also omits person and stores principal.actor as the author.
- `crates/fleetdeck-deck/src/http/people_routes.rs:155` — The mob-session sibling invokes person before session_of, showing the available intended check.
- `crates/fleetdeck-deck/src/store/chat.rs:1` — The store module documents side chat as people-only and not agent-readable.

Why it remains unconfirmed:

- Target execution is prohibited because the required isolated-loopback and cumulative-disk sandbox controls are unavailable, so no dummy bearer request was observed.

Safe validation plan:

- **Local:** Inside an approved isolated-loopback sandbox, create a dummy session and chat row, mint a Read token and call GET /api/sessions/{station}/{session}/chat, then mint a Drive token and call POST on the same route. Confirm whether the current build returns or inserts the dummy row; a fixed regression must return 403 for tokens while owner and covering-member cookie principals still work.

### NV-08: Inline provider credentials may reside in a readable station configuration

- **Fingerprint:** `config.inline-provider-secret-readable-mode`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** station.toml may contain plaintext provider environment values that commonly hold API keys, while its loader accepts mode 0644. If such a value is deployed and the configuration's ancestors are traversable by a lower-trust local account, that account can read the credential.
- **Claimed root cause:** The configuration retains a plaintext inline-secret form while read_owner_file enforces integrity but not confidentiality; referenced secret files, in contrast, require mode 0600.

Source trace:

1. **entrypoint — RawEnvValue:** `crates/fleetdeck-core/src/config.rs:316` — A provider environment value may be a plaintext string in station.toml.
2. **propagation — parse_instances:** `crates/fleetdeck-core/src/config.rs:523` — Plain configuration text is accepted as the provider environment value and wrapped only after file parsing.
3. **sink — read_owner_file:** `crates/fleetdeck-core/src/config.rs:698` — The loader rejects only group/other write bits, so a mode-0644 file containing the value is accepted.

Verified source evidence:

- `crates/fleetdeck-core/src/config.rs:10` — The module documents that provider environment values often hold API keys and may be plain strings.
- `crates/fleetdeck-core/src/config_tests.rs:299` — A regression test explicitly sets station.toml to 0644 and verifies that loading succeeds.
- `crates/fleetdeck-core/src/settings/refs.rs:28` — Referenced secret files must be regular, non-symlink files with exactly mode 0600.

Why it remains unconfirmed:

- Source does not show whether a deployed station.toml contains an inline credential rather than only secret references.
- Deployed file and ancestor owner, mode, ACL, and lower-trust local-account access are not source-visible.
- The required execution sandbox is unavailable.

Safe validation plan:

- **Owner-observed deployment check:** As the owner, inspect station.toml without printing values: record whether any provider_instance env value is a plaintext string, record owner/mode/ACL metadata for the file and every ancestor, and verify with a designated unprivileged account whether the path is traversable and readable.

### NV-09: Third-party integration error text reaches coordinator MCP output without the untrusted envelope

- **Fingerprint:** `coordinator.integration-error-untrusted-text-unfenced`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** A connected forge or tracker can place its failed-response text in a coordinator project-items tool result outside the repository's untrusted-content envelope. The text is credential-scrubbed and capped at 500 characters, but it is returned as ordinary MCP error/result text to a model that may also hold manage or answer tools. Existing class, target, mode, approval and live-authority checks bound any later action, but they do not tell whether that action was induced by the unfenced error. Whether a supported model will follow an attacker-chosen error instruction and invoke an otherwise authorized mutation requires bounded behavioral validation.
- **Claimed root cause:** The coordinator read bridge applies fence/fence_cut only while shaping successful station values. Station failures instead flow through station_refusal as raw formatted Reject.message text; integration helpers deliberately preserve a scrubbed prefix of remote response/error bodies, so lower-trust remote text bypasses the provenance label required for coordinator-visible third-party text.

Source trace:

1. **entrypoint — Forge::call:** `crates/fleetdeck-station/src/integrations/forge.rs:78` — A failed gh/glab request incorporates the remote response body and helper stderr into a Fail::upstream message.
2. **propagation — Fail::upstream and scrub:** `crates/fleetdeck-station/src/integrations/exec.rs:49` — The failure text is credential-scrubbed and cut to 500 characters, but it is not wrapped with the untrusted-content fence.
3. **propagation — Integrations::call:** `crates/fleetdeck-station/src/integrations/mod.rs:151` — Integration dispatch converts Fail into Outcome::Err while preserving fail.message for the deck read request.
4. **propagation — station_refusal:** `crates/fleetdeck-deck/src/link/coordinator_reads.rs:53` — The coordinator read bridge formats code and station message directly into Reject.message; unlike successful payload shaping, this path does not call fence or fence_cut.
5. **propagation — coordinator_query:** `crates/fleetdeck-deck/src/link/requests.rs:88` — The station-read rejection is returned as a successful query value containing the Reject object.
6. **sink — tools/call response:** `crates/fleetdeck-station/src/coordinator/jsonrpc.rs:100` — The returned value is serialized verbatim into MCP text delivered to the coordinator model, which may have separately class-granted mutation tools.

Verified source evidence:

- `docs/v2/adr/ADR-008-fleet-control.md:72` — The accepted design requires every coordinator-visible text it did not write, including issue and pull-request query results, to use the untrusted envelope.
- `crates/fleetdeck-proto/src/untrusted.rs:18` — The intended fence adds an explicit third-party-data notice, sanitizes source metadata and prevents an embedded closing tag from ending the envelope.
- `crates/fleetdeck-deck/src/link/coordinator_reads.rs:103` — Successful diff, file, search, project-item and artifact values are routed through explicit shaping and fences, showing the stronger control that the refusal path bypasses.
- `crates/fleetdeck-station/src/integrations/http.rs:160` — A non-success tracker HTTP response contributes its remote body to the scrubbed upstream failure message.
- `crates/fleetdeck-station/src/integrations/exec.rs:280` — Scrubbing joins whitespace and caps the message, providing secrecy and size bounds but no untrusted provenance label.
- `crates/fleetdeck-deck/src/store/coordinator_ops.rs:255` — A later mutation remains constrained by its grant classes and downstream target checks; this is the strongest visible preventing layer and limits the candidate to otherwise authorized effects.

Why it remains unconfirmed:

- This assignment forbids target-code execution, and source alone cannot establish whether a supported coordinator model will treat an attacker-chosen MCP error message as instructions and issue an unrequested mutation within its existing grant.

Safe validation plan:

- **Local:** Inside the required offline OS sandbox with dummy state only, replace gh/glab or curl with a fixture helper that returns a failed response body containing a unique instruction, invoke project_items through a coordinator binding that has inspect plus one narrowly scoped dummy mutation tool, and capture the MCP response to verify the fixture appears without <untrusted>. Then run the supported local model/eval harness against that response and record whether it issues the named mutation; keep all stations, sessions and effects synthetic and block external network access.

### NV-10: Terminal coordinator outbox rows have no retention bound

- **Fingerprint:** `coordinator.outbox-terminal-retention-unbounded`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** An authenticated coordinator binding can submit syntactically valid mutating tool calls within a 256 KiB body, 20-per-second rate, and eight-call concurrency limit. The station persists the complete envelope before the deck's semantic decision. Settlement, expiry, fencing, and restart recovery make rows terminal but do not delete them, and the normal prune transaction omits coordinator_outbox. The missing application retention bound is source-visible; deployed disk capacity and bounded growth evidence remain necessary to establish shared availability impact.
- **Claimed root cause:** The durable coordinator outbox retains full attacker-influenced envelopes in terminal states indefinitely and is not covered by station pruning, a row/byte quota, or an age policy.

Source trace:

1. **entrypoint — coordinator::ingress::handle:** `crates/fleetdeck-station/src/coordinator/ingress.rs:204` — An authenticated binding submits a bounded MCP tools/call request.
2. **propagation — LinkForward::durable_call:** `crates/fleetdeck-station/src/coordinator/outbox.rs:152` — The complete envelope is serialized and passed to coordinator_intake before first transmission.
3. **propagation — Store::coordinator_intake:** `crates/fleetdeck-station/src/store/coordinator_outbox.rs:208` — The envelope is inserted as a durable pending row.
4. **propagation — Store::coordinator_settle:** `crates/fleetdeck-station/src/store/coordinator_outbox.rs:276` — Settlement changes state and optionally stores a receipt but leaves the original envelope and row.
5. **sink — schema V17 coordinator_outbox:** `crates/fleetdeck-station/src/store/schema.rs:413` — Terminal rows and their envelope text remain in the shared station SQLite table without a retention constraint.

Verified source evidence:

- `crates/fleetdeck-station/src/coordinator/ingress.rs:53` — One request body is capped at 256 KiB.
- `crates/fleetdeck-station/src/coordinator/bindings.rs:24` — A binding is limited to eight concurrent and twenty requests per second, which does not bound retained lifetime bytes.
- `crates/fleetdeck-station/src/store/coordinator_outbox.rs:293` — Expiry updates pending rows to terminal states rather than deleting them.
- `crates/fleetdeck-station/src/store/log.rs:295` — Normal pruning removes events and command receipts but does not reference coordinator_outbox.
- `crates/fleetdeck-deck/src/store/grants.rs:33` — A grant can live for at most seven days, limiting creation time but not terminal retention.

Why it remains unconfirmed:

- Target execution is prohibited because the full required sandbox is unavailable, so no bounded dummy SQLite lifecycle fixture measured retained rows or bytes.
- Station disk quota, real coordinator call cadence, database maintenance, and supervisor blast radius are deployment facts needed to quantify meaningful availability impact.

Safe validation plan:

- **Local:** In an approved scratch-only sandbox, use a dummy binding and transport to submit a small fixed number of valid mutating calls whose harmless large text the dummy deck rejects. Settle each row, advance a fake clock, invoke Store::prune, and compare only row counts and database size before and after under strict limits.
- **Owner-observed deployment check:** The owner may run a read-only count and sum(length(envelope)) grouped by coordinator_outbox state and compare historical database growth with filesystem quotas; do not create calls or fill disk.

### NV-11: Unauthenticated refusals can grow the durable deck audit log without retention

- **Fingerprint:** `deck.audit.unauthenticated-refusal-unbounded-retention`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** An unauthenticated network client can repeatedly select a recognized mutating API route or public authentication flow. Guard and limiter refusals append durable audit rows, but the audit table has no source-visible retention or aggregate quota. Sequential low-byte requests can therefore grow the shared deck database; actual availability impact depends on bounded measurements and deployment storage controls.
- **Claimed root cause:** Pre-authentication refusal paths append one persistent audit row per request, including authentication-limit refusals, while no source owner deletes or caps audit rows by age, count, or bytes.

Source trace:

1. **entrypoint — guard:** `crates/fleetdeck-deck/src/http/guard.rs:415` — A remote unauthenticated client can select a recognized mutating board route and omit valid credentials.
2. **propagation — guard refusal branch:** `crates/fleetdeck-deck/src/http/guard.rs:434` — Every failed guard check on a mutating route awaits audit_or_log and defaults the absent actor to anonymous.
3. **propagation — limit_refusal:** `crates/fleetdeck-deck/src/http/auth_routes.rs:181` — Public authentication flow-limit refusals also append an audit row for each rejected request.
4. **sink — DeckStore::audit:** `crates/fleetdeck-deck/src/store/audit.rs:107` — The sink executes an unconditional INSERT into the persistent audit table.

Verified source evidence:

- `crates/fleetdeck-deck/src/http/admission.rs:31` — Connection admission bounds simultaneous connections but not sequential request lifetime cardinality.
- `crates/fleetdeck-deck/src/store/audit.rs:136` — The only audit-log lifecycle operation shown is a bounded newest-row read, not deletion.
- `crates/fleetdeck-deck/src/store/schema.rs:157` — The audit table has an autoincrement key and no expiry, cardinality, or retention constraint.

Why it remains unconfirmed:

- The required isolated-loopback, cumulative-disk-limited OS sandbox is unavailable, so request-to-row growth, database/WAL growth, latency, and recovery near a safe storage threshold were not observed.
- Listener reachability, reverse-proxy request budgets, filesystem quotas, supervisor isolation, and available storage are deployment facts.

Safe validation plan:

- **Local:** In an approved isolated-loopback sandbox with a tiny disposable filesystem and strict request, time, memory, process, output, and cumulative disk limits, send small fixed batches of unauthenticated requests to one recognized mutating route and invalid login requests beyond the limiter. Record audit rows and database/WAL bytes after each batch, then stop well before disk pressure.
- **Owner-observed deployment check:** Without generating traffic, document board listener reachability, upstream sustained request budgets, deck database quota/alerts, and disk-full recovery policy.

### NV-12: A superseded station link can commit a stale snapshot over newer deck state

- **Fingerprint:** `event-sync.superseded-stale-snapshot-commit`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** An authenticated station can have an old, internally valid snapshot batch in the deck's application queue when a higher-epoch or reciprocal winning link supersedes it. The winning link can advance the station's deck cursor and create newer durable projections before the old link task drains its queued SnapshotEnd. Because snapshot application checks only the batch's internal metadata and not its freshness or link generation, the old batch can then replace the newer read model and lower the durable cursor. Sessions created after the old watermark are removed, their open deck tasks become permanently blocked, and their deck-owned review comments and review records are deleted, even though replay can later reconstruct the station summary.
- **Claimed root cause:** Deck synchronization has no store-enforced monotonic snapshot rule and no application-level binding between Sync and the currently winning LinkId. LinkTable correctly supersedes the old transport and the control supervisor aborts its reader, but already queued Inbound frames remain in the receiver; DeckLink::serve processes one received frame before checking control.ended. Sync::end_snapshot validates only begin/end equality and DeckStore::apply_snapshot_with unconditionally deletes sessions and derived rows and writes the supplied incarnation and watermark, even when the stored same-incarnation cursor is newer.

Source trace:

1. **entrypoint — control::on_frame:** `crates/fleetdeck-link/src/control.rs:713` — A direction- and feature-checked SnapshotEnd from the authenticated station is placed in the bounded application queue of the old link.
2. **propagation — LinkTable::insert:** `crates/fleetdeck-link/src/table.rs:139` — A newer winning link replaces the old per-peer entry and signals the old control channel as superseded, but replacement does not invalidate application messages already queued for that old link.
3. **propagation — DeckLink::serve inbound loop:** `crates/fleetdeck-deck/src/link/mod.rs:198` — Each link owns an independent Sync. The loop processes the next queued frame and checks control.ended only afterward, so one queued SnapshotEnd can still reach the old Sync after supersession while the replacement Sync has already advanced deck state.
4. **propagation — Sync::end_snapshot:** `crates/fleetdeck-deck/src/link/mod.rs:372` — The old Sync requires its SnapshotEnd to match that Sync's begin incarnation, watermark and count, but does not compare the batch to the station's current cursor or current winning link before applying it.
5. **propagation — DeckStore::apply_snapshot_with:** `crates/fleetdeck-deck/src/store/read_model.rs:286` — Inside one transaction the store reloads the station row but performs no monotonic-watermark or stream-generation check, deletes all current sessions, blocks tasks for sessions absent from the stale batch, applies optional grant projection and unconditionally sets the cursor to the supplied old watermark.
6. **sink — review::delete_unlisted:** `crates/fleetdeck-deck/src/store/review.rs:341` — Snapshot application permanently deletes comments, reviews, revisions, imports and attempts for every newer session absent from the stale batch; later event replay does not reconstruct these deck-owned rows.

Verified source evidence:

- `crates/fleetdeck-link/src/control.rs:394` — Each control channel has a bounded mpsc application queue, distinct from the socket reader and supervisor.
- `crates/fleetdeck-link/src/control.rs:515` — Supersession aborts the socket reader and marks the control ended, but does not clear or close the receiver's already queued Inbound values.
- `crates/fleetdeck-deck/src/link/mod.rs:218` — DeckLink::serve tests whether the control ended only after dispatching the received request or frame.
- `crates/fleetdeck-deck/src/link/mod.rs:381` — Snapshot completion checks only equality with the old Sync's OpenSnapshot and then calls apply_snapshot_with without a link token or expected current cursor.
- `crates/fleetdeck-deck/src/store/read_model.rs:297` — The snapshot transaction reads the current station record, but uses it only to decide whether to clear timeline/read cursors for an incarnation change; it never rejects a stale same-incarnation watermark or obsolete incarnation.
- `crates/fleetdeck-deck/src/store/read_model.rs:310` — Snapshot application deletes the station's current sessions before inserting only those named by the incoming batch.
- `crates/fleetdeck-deck/src/store/fleet.rs:289` — Every open or question task whose session is absent from the snapshot is irreversibly changed to blocked with completed_at set.
- `crates/fleetdeck-deck/src/store/read_model.rs:338` — The same snapshot transaction deletes review data of sessions absent from the batch and then writes the supplied cursor.
- `crates/fleetdeck-deck/src/store/review_tests.rs:387` — The existing source test establishes that omitting a session from a snapshot deletes that session's comments, reviews and revisions; it does not cover stale snapshot ordering.
- `crates/fleetdeck-deck/src/store/read_model.rs:239` — The contrasting event path commits every derived mutation and its cursor in one transaction, and duplicate events at or below that cursor are skipped.
- `crates/fleetdeck-deck/src/link/mod.rs:405` — The deck sends a snapshot Ack only after apply_snapshot_with returns, so commit ordering is correct but does not compensate for accepting a stale commit.

Why it remains unconfirmed:

- Target execution was prohibited because the required OS-enforced sandbox controls were unavailable: empty allowlisted environment, disabled external networking, read-only target and toolchain, scratch-only writes, and explicit CPU, memory, process, file-size, disk, and wall-clock limits. The empty artifact promotion allowlist also precluded retained observed evidence.

Safe validation plan:

- **Local:** Inside the required sandbox, extend the existing fixed-size fake-station loopback fixture with deterministic barriers. On the old link, send SnapshotBegin and its complete session list at watermark W, enqueue SnapshotEnd, and pause the old DeckLink application task. Admit a higher-epoch replacement link for the same dummy station; through it commit events through N greater than W that create one dummy session. Add one dummy review comment and one open task for that newer session. Release the old task and verify that it consumes the queued end, acknowledges W, lowers the stored cursor from N to W, removes the newer session, deletes its review rows and marks its task blocked. Stop at those rows. A regression should bind mutations to the current link generation and reject or no-op same-incarnation snapshots below the committed cursor; repeat the schedule and require cursor N and all newer deck-owned rows to remain unchanged.

### NV-13: Global authentication failure windows can deny unrelated sources

- **Fingerprint:** `http.auth.global-failure-window-lockout`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** Password login, login-link redemption, and member join each use a FlowLimiter with five failures per source key and fifty failures globally per minute. After the global list fills, begin rejects every source in that flow. The password limiter is also consulted by authenticated owner elevation, so anonymous password failures can prevent a fresh owner source from entering elevation. This is an explicit availability-versus-abuse-control design, and a bounded deterministic fixture is required to establish the exact lockout behavior before treating it as a defect.
- **Claimed root cause:** Failures created by unrelated unauthenticated sources share a flow-wide admission window, and the public password flow shares that global window with authenticated owner elevation.

Source trace:

1. **entrypoint — auth_routes::password:** `crates/fleetdeck-deck/src/http/auth_routes.rs:245` — An unauthenticated client submits an invalid password.
2. **propagation — FlowLimiter::begin:** `crates/fleetdeck-deck/src/http/admission.rs:315` — Each admitted attempt reserves both a per-source and global entry.
3. **propagation — auth_routes::login:** `crates/fleetdeck-deck/src/http/auth_routes.rs:159` — Invalid credentials retain the reservation as a failure.
4. **propagation — Windows::locked:** `crates/fleetdeck-deck/src/http/admission.rs:254` — A full global window returns a wait regardless of the requesting source key.
5. **sink — auth_routes::elevate:** `crates/fleetdeck-deck/src/http/auth_routes.rs:521` — Owner elevation checks the same password limiter and can return 429 before verifying an otherwise valid owner challenge.

Verified source evidence:

- `crates/fleetdeck-deck/src/http/admission.rs:37` — The source declares five failures per source and fifty globally per flow.
- `crates/fleetdeck-deck/src/http/admission.rs:59` — IPv4 addresses and IPv6 /64 prefixes form source keys.
- `crates/fleetdeck-deck/src/http/admission.rs:393` — Password, login-link, and member-join use separate FlowLimiter instances.
- `crates/fleetdeck-deck/src/http/auth_routes.rs:347` — Member join begins in the member_join limiter and retains an invalid invite as a failure.

Why it remains unconfirmed:

- Target execution is prohibited because the required network-isolated, empty-environment, read-only-target, scratch-only, fully resource-limited sandbox is unavailable.
- The source intentionally documents the global cap, so a deterministic fixture and product availability requirement are needed to distinguish an exploitable lockout from the accepted abuse-protection tradeoff.

Safe validation plan:

- **Local:** In an approved in-memory fixture with a dummy clock, submit five invalid attempts from each of ten source keys to one FlowLimiter, then attempt a valid request from an eleventh key and record the 429 and wait. Repeat for password elevation with an authenticated dummy owner. Stop after these bounded transitions and use no live listener or real credential.
- **Owner-observed deployment check:** The owner should document whether another owner-local recovery/elevation path remains usable during the password window and whether upstream source aggregation or trusted ingress materially changes the set of reachable source keys.

### NV-14: Tracker asset credentials are authorized by hostname rather than HTTPS origin

- **Fingerprint:** `integrations.asset.auth-origin-port-confusion`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** A Drive-scoped caller may provide an asset URL on an explicit secondary port of the configured tracker hostname. Validation discards the port for binding and credential-recipient comparisons while curl receives the original URL and Authorization header. A distinct lower-trust TLS service reachable on that host and port could therefore receive the station-held tracker credential.
- **Claimed root cause:** Asset authorization compares lowercase hostname strings rather than normalized HTTPS origins, discarding the explicit port before selecting both the allowed target and Authorization recipient.

Source trace:

1. **entrypoint — projects_routes::asset:** `crates/fleetdeck-deck/src/http/projects_routes.rs:525` — The Drive-scoped route accepts a caller-controlled asset URL and forwards it with the project's stored binding.
2. **propagation — AssetQuery::check:** `crates/fleetdeck-proto/src/integrations/checks.rs:234` — The shared check requires HTTPS and bounded text but does not bind the URL port to the tracker origin.
3. **propagation — https_host:** `crates/fleetdeck-station/src/integrations/util.rs:199` — The helper validates the authority but returns only the hostname before target and auth-recipient comparisons.
4. **propagation — asset::fetch:** `crates/fleetdeck-station/src/integrations/asset.rs:49` — The initial URL remains unchanged and receives auth headers when its stripped hostname matches.
5. **sink — http::config:** `crates/fleetdeck-station/src/integrations/http.rs:51` — The curl config serializes the original URL followed by the selected Authorization header.

Verified source evidence:

- `crates/fleetdeck-proto/src/integrations/checks.rs:11` — valid_host explicitly permits an optional numeric port.
- `crates/fleetdeck-station/src/integrations/asset.rs:95` — Host allowlisting and auth selection compare hostname strings only.
- `crates/fleetdeck-station/src/integrations/asset.rs:140` — The non-443 redirect rejection is bypassed whenever the stripped hostname is already in the binding host list.
- `crates/fleetdeck-deck/src/http/methods_table_rest.rs:235` — The asset endpoint is authorized to Drive scope rather than only administrative callers.

Why it remains unconfirmed:

- The required sandbox is unavailable, so a dummy fake-curl fixture could not independently observe the generated URL/header pair.
- Source does not establish that a supported deployment exposes a valid-TLS service controlled by a lower-trust principal on another port of a configured tracker hostname.

Safe validation plan:

- **Local:** In the required network-free sandbox, use a fake curl executable that captures only its stdin config. Configure a disposable Jira binding with a dummy token, request https://tracker.test:8443/pixel.png, and verify whether both that URL and Authorization appear. Validate an origin-aware fix by requiring the configured normalized scheme-host-port tuple.
- **Owner-observed deployment check:** Without probing third-party systems, the owner should inventory whether configured tracker hostnames have non-443 HTTPS services reachable by Drive-scoped users and whether a different principal controls any such service.

### NV-15: A failed durable revocation is reported as success and can revive after restart

- **Fingerprint:** `link.revocation-store-failure-peer-revival`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** The shared link revocation helper deliberately continues after the PairingSink fails: it logs the store error, unpins the peer in memory and closes current links. The application-level deck remove-station and station forget-deck workflows do not receive that error and return successful removed/forgotten responses. If the persistent row was not deleted, a later node start reloads and re-pins it. The station-side variant is the strongest boundary: a deck that the owner believes forgotten retains its private key and, after restart, can again receive Read, Drive, Approve and Admin authority, including session, file, integration and settings methods allowed by policy. A transient SQLite write failure that later recovers and a subsequent restart are required; source-only review did not reproduce that lifecycle.
- **Claimed root cause:** fleetdeck-link::revoke collapses a durable sink failure into a log message and returns only whether the in-memory key was pinned, so callers cannot make success contingent on persistence; startup then treats the unchanged persistent pairing row as authoritative and reconstructs the revoked pin and grants.

Source trace:

1. **entrypoint — local::pairing:** `crates/fleetdeck-station/src/local.rs:274` — The owner invokes the local ForgetDeck operation and receives a Forgotten count when StationNode::forget_deck returns Ok.
2. **propagation — StationNode::forget_deck:** `crates/fleetdeck-station/src/link/runtime.rs:626` — The station loads all persistent deck keys, calls the shared revoke helper for each, ignores its boolean result, increments the count and returns success.
3. **propagation — revoke:** `crates/fleetdeck-link/src/revoke.rs:54` — A PairingSink/store error is only logged; the helper still removes the in-memory pin, closes the live link and returns without exposing the error to the application.
4. **propagation — StationPeers::revoked:** `crates/fleetdeck-station/src/link/peers.rs:124` — The cache is revoked before Store::revoke_peer; if the store call fails, the persistent pairing row remains even though current-process checks fail closed.
5. **propagation — Store::revoke_peer:** `crates/fleetdeck-station/src/store/peers.rs:186` — Durable deletion and revocation-generation increment are transactional, but any write error leaves the old pairing row intact.
6. **sink — StationNode::start_with_dirs:** `crates/fleetdeck-station/src/link/runtime.rs:238` — A fresh station process reloads paired_keys from the unchanged database into the grant cache and PinSet, restoring the old deck key's authority and dialer.

Verified source evidence:

- `crates/fleetdeck-link/src/revoke.rs:50` — The helper documentation explicitly states that a sink error is logged while the key is unpinned and its link is closed anyway.
- `crates/fleetdeck-station/src/link/runtime.rs:47` — A paired deck receives Read, Drive, Approve and Admin scopes on the station, making restored authority security-relevant.
- `crates/fleetdeck-station/src/store/peers.rs:75` — Mutating decisions re-check the persistent pairing row and scope; that strong current-process control cannot distinguish a revocation whose store write was suppressed.
- `crates/fleetdeck-station/src/link/peers.rs:41` — StationPeers::load reconstructs the grant cache from every stored pairing row.
- `crates/fleetdeck-station/src/link/peers.rs:80` — pin_all reconstructs TLS pins from those same stored rows at node start.
- `crates/fleetdeck-deck/src/link/runtime.rs:401` — The deck-side remove_station path has the same root cause: it awaits revoke but cannot observe a failed durable station deletion and returns the old record as removed.
- `crates/fleetdeck-deck/src/peers.rs:139` — The deck revokes cache state first and then may return an error from revoke_station to the shared helper, which suppresses it.
- `crates/fleetdeck-deck/src/http/station_routes.rs:165` — The owner HTTP route maps remove_station Ok to a successful response and audit result; the suppressed sink error cannot reach it.

Why it remains unconfirmed:

- Target execution was prohibited, so no bounded test injected a transient SQLite write failure after the persistent key lookup and then restarted a node against the unchanged dummy database.
- The source proves stale durable state is reloaded, but the deployed supervisor's restart timing and recovery from the initiating database error are external lifecycle facts.
- No production logs or databases were inspected to determine whether a revocation-store error has occurred in practice.

Safe validation plan:

- **Local:** Add a test-only failpoint or a dummy PairingSink/store wrapper in an isolated fixture: persist a dummy paired deck, fail exactly the revoke write, call forget_deck and record the returned success, then construct a fresh station node from the same dummy database and assert whether the old key is pinned and regains its prior scopes. Run the symmetric deck remove_station case. Fix by returning the sink error through revoke and refusing success until durable deletion commits, then rerun the restart check.
- **Owner-observed deployment check:** The owner should search existing service logs for revocation store errors and compare any affected fingerprint with the current paired-peer rows and revocation generation, without reconnecting or exercising that peer. Verify supervisor restart behavior only from configuration and historical records, not by inducing a production database failure.

### NV-16: Reissued bearer tokens inherit revoked-token read cursors

- **Fingerprint:** `metadata.read-cursor-token-name-reuse`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** A read-scoped token can advance a session cursor to any nonnegative signed-64-bit sequence. Although a recreated token with the same name is documented as a different principal by hash, read cursors use token:<name> and survive revocation. The successor therefore inherits the predecessor's cursor, which can suppress unread indicators for later messages, approvals, and plans.
- **Claimed root cause:** Read-cursor state uses the audit display label token:<name> rather than the immutable token hash or issuance identifier, and token revocation does not remove name-keyed cursor rows.

Source trace:

1. **entrypoint — mark_read:** `crates/fleetdeck-deck/src/http/activity_routes.rs:27` — A read-scoped principal supplies a session and any sequence within the signed 64-bit range.
2. **propagation — principal_key:** `crates/fleetdeck-deck/src/store/read_cursors.rs:21` — Cursor state is keyed by Principal::actor rather than the exact bearer issuance identity.
3. **propagation — Principal::actor:** `crates/fleetdeck-deck/src/store/auth.rs:107` — Every token issuance with the same name maps to token:<name>, omitting the hash stored in Principal::Token.
4. **sink — DeckStore::sessions_for:** `crates/fleetdeck-deck/src/store/read_cursors.rs:108` — The successor token loads unread counts through the colliding key, so the predecessor's maximum cursor suppresses later unread state.

Verified source evidence:

- `crates/fleetdeck-deck/src/store/auth.rs:94` — Principal::Token retains the token hash and documents same-name recreation as another principal.
- `crates/fleetdeck-deck/src/store/auth.rs:505` — Revocation deletes only api_tokens by name and does not delete read_cursors.
- `crates/fleetdeck-deck/src/store/read_cursors.rs:69` — mark_read upserts the maximum sequence under principal, station, and session.
- `crates/fleetdeck-deck/src/store/read_cursors.rs:15` — The cursor controls unread counts for completed messages, approval requests, and proposed plans.

Why it remains unconfirmed:

- The required sandbox is unavailable, so the two-issuance dummy-store sequence and resulting unread count were not independently observed.

Safe validation plan:

- **Local:** In the required sandbox with an in-memory DeckStore, create a read token named phone, mark a dummy session at i64::MAX, revoke it, recreate phone with a different value/hash, append one bounded unread event, and assert the second principal incorrectly reports no unread item. Verify a fix keys cursor state by an immutable issuance identity or deletes it transactionally on revocation.

### NV-17: Queued member mail can outlive share revocation

- **Fingerprint:** `orchestration.member-mail-stale-share-authority`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** A collaborator can submit a follow-up or steer to a shared session while its share and, in mob mode, driver status are valid. The deck converts that action into durable outbox intent valid for up to 24 hours, but the item carries neither the member session nor share authority. Revoking the share immediately deletes its invites and member sessions, yet leaves the queued item. When the station later becomes reachable, the outbox revalidates only coordinator items with a populated CommandAuthority, so source indicates that the revoked member's queued action can still be delivered to the owner's session.
- **Claimed root cause:** Member-authorized mail loses its share/member/driver provenance when converted into a generic NewOutboxItem, and share revocation plus outbox attempt gates have no control that cancels or reauthorizes that delayed item.

Source trace:

1. **entrypoint — send_mail:** `crates/fleetdeck-deck/src/http/fleet_routes.rs:144` — POST /api/mail accepts a member principal, checks the current shared session, role and mob-driver state, then constructs a follow-up or steer mail intent.
2. **propagation — send_mail:** `crates/fleetdeck-deck/src/http/fleet_routes.rs:185` — The accepted member action becomes a generic NewOutboxItem with a 24-hour expiry; no share, member login or driver identity is carried forward.
3. **propagation — insert_mail:** `crates/fleetdeck-deck/src/store/fleet.rs:178` — The mail and queued outbox item are committed together, but insert_item leaves the outbox authority column unset.
4. **propagation — revoke_share:** `crates/fleetdeck-deck/src/store/shares.rs:208` — Revocation marks the share revoked and deletes its invites and member sessions, but performs no cancellation of mail or open outbox rows created by that share.
5. **propagation — start_attempt:** `crates/fleetdeck-deck/src/store/outbox.rs:434` — A queued, unexpired item is admitted for an attempt; current authorization is rechecked only when the optional coordinator authority column is populated, which an ordinary member mail never does.
6. **sink — DeckLink::attempt:** `crates/fleetdeck-deck/src/link/outbox.rs:59` — The generic mail payload is rebuilt as a routed command and submitted to the station using the stored expiry and absent authority, permitting delayed delivery after revocation.

Verified source evidence:

- `crates/fleetdeck-deck/src/http/access.rs:64` — The strongest initial control checks that the live member grant covers the requested station/session and permits MemberAction::Mail; fleet_routes also checks current mob-driver state before enqueue.
- `crates/fleetdeck-deck/src/http/fleet_routes.rs:99` — intent constructs only station, origin, serialized command and a 24-hour expires_at value, with no authorizing share or member identity.
- `crates/fleetdeck-deck/src/store/outbox.rs:113` — OutboxItem documents authority as present only for fleet-control intent; every other item, including member mail, uses None.
- `crates/fleetdeck-deck/src/store/shares.rs:203` — The stated revoke operation ends the share, invite and member logins at once, but its transaction touches only shares, invites and member_sessions.
- `crates/fleetdeck-deck/src/store/outbox.rs:455` — The strongest delayed-effect control performs current grant/target rechecks only inside if let Some(authority); a NULL-authority item bypasses the recheck and is inserted as a pending command.

Why it remains unconfirmed:

- The assignment is source-only and the host lacks the required cumulative disk quota and usable isolated loopback sandbox controls, so target code could not be executed to observe a dummy queued mail crossing a share revoke/reconnect sequence.

Safe validation plan:

- **Local:** In the required isolated sandbox with an in-memory dummy deck, create a collaborator share for one session, keep its station link offline, enqueue one POST /api/mail as that member, revoke the share, then attach a fake station link and run one outbox tick. Assert that the current code emits the stored FollowUp or Steer after revocation; a fixed regression must instead mark the item dead/cancelled and emit no command. Repeat with share expiry and a mob-driver handoff to determine which authority transitions the product intends to invalidate.

### NV-18: Closed provider requests accumulate in durable adapter checkpoints

- **Fingerprint:** `provider-adapter-checkpoint-unbounded-request-state`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** Provider-originated approval requests are retained in adapter correlation state after they close. Claude marks answered or cancelled requests in place and keeps native-to-app mappings; Codex moves closed requests into an unbounded closed map. Durable hosting serializes the complete core after every accepted provider line and replaces the station's checkpoint text, which recovery later decodes in full. A bounded sans-I/O fixture is needed to prove monotonic checkpoint growth and shared station impact.
- **Claimed root cause:** Provider adapters keep terminal request and correlation records without count, byte, age, or lifecycle eviction while the durable driver snapshots the entire adapter state after each line.

Source trace:

1. **entrypoint — on_control_request:** `crates/fleetdeck-providers/src/claude/requests.rs:88` — A provider-controlled request with a fresh native id creates a runtime request and reverse mapping.
2. **propagation — on_cancel:** `crates/fleetdeck-providers/src/claude/requests.rs:165` — Cancellation marks the request expired but does not remove either retained map entry.
3. **propagation — DurableDriver output pump:** `crates/fleetdeck-station/src/manager/durable.rs:590` — Each complete provider line mutates the core and snapshots its complete state.
4. **sink — store_checkpoint:** `crates/fleetdeck-station/src/store/shims.rs:601` — The serialized checkpoint replaces adapter_checkpoint in the station database and is loaded in full during restore.

Verified source evidence:

- `crates/fleetdeck-providers/src/claude/core.rs:165` — ClaudeCore serializes requests and native_to_app as checkpoint fields.
- `crates/fleetdeck-providers/src/codex/requests.rs:58` — Codex Requests serializes open, by_native, and closed collections without eviction metadata.
- `crates/fleetdeck-providers/src/codex/requests.rs:463` — Closing a request moves a terminal marker into the closed map.
- `crates/fleetdeck-station/src/manager/durable.rs:212` — Recovery reads and deserializes the stored checkpoint text in full.
- `crates/fleetdeck-providers/src/host/shim/output.rs:21` — Shim buffering caps an individual outstanding byte span, not cumulative adapter state across bounded lines.

Why it remains unconfirmed:

- Target execution is prohibited because the complete required sandbox is unavailable, so no bounded sans-I/O request sequence or checkpoint-size measurement was run.
- The practical rate of provider-generated distinct terminal requests and the deployment memory/database/restart blast radius are not established by source.

Safe validation plan:

- **Local:** In an approved sandbox, drive the sans-I/O Claude and Codex cores with a small fixed sequence of unique dummy approval requests followed by cancellation or resolution. At several bounded counts, serialize checkpoints and record collection counts and byte lengths, then restore one checkpoint through an in-memory store. Stop after monotonic retention is shown.

### NV-19: Provider-driven replies can accumulate in an unbounded stdin queue

- **Fingerprint:** `provider-stdin-unbounded-response-queue`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** Claude and Codex sessions place all stdin frames on unbounded channels. A lower-trust provider can emit repeated protocol requests that generate automatic replies while it stops reading stdin. The reader can continue enqueueing JSON values while the sole writer blocks on the pipe, retaining unbounded memory in the shared station process.
- **Claimed root cause:** Protocol-generated writes share an unbounded MPSC queue and reader-originated responses have neither queue backpressure nor the flush timeout used by host-initiated commands.

Source trace:

1. **entrypoint — on_control_request:** `crates/fleetdeck-providers/src/claude/requests.rs:144` — Provider-originated unsupported hook or MCP control requests generate automatic Output::Write refusal frames.
2. **propagation — Shared::dispatch:** `crates/fleetdeck-providers/src/claude/driver.rs:385` — Every generated response is sent to an UnboundedSender without a capacity check.
3. **sink — write_loop:** `crates/fleetdeck-providers/src/claude/driver.rs:424` — The sole consumer awaits writing one frame to provider stdin; queued frames remain retained if the provider does not read.

Verified source evidence:

- `crates/fleetdeck-providers/src/stdio.rs:1` — The shared stdin queue is explicitly documented as unbounded.
- `crates/fleetdeck-providers/src/claude/driver.rs:203` — Claude creates mpsc::unbounded_channel for all stdin writes.
- `crates/fleetdeck-providers/src/codex/driver.rs:220` — Codex independently uses the same unbounded queue architecture.
- `crates/fleetdeck-providers/src/stdio.rs:37` — The flush timeout applies only when a caller explicitly invokes flush; automatic reader responses are enqueued without it.

Why it remains unconfirmed:

- The required OS sandbox is unavailable, so a fake provider that withholds stdin reads could not be used to observe bounded queue growth and isolation from another dummy session.

Safe validation plan:

- **Local:** In the complete required sandbox, run a fake provider that never reads stdin and emits a small fixed sequence of unsupported control requests. Instrument queue depth or use a deliberately tiny bounded queue in a regression fixture; verify current accumulation, then verify overflow causes prompt session-local termination or backpressure without affecting a second dummy session.

### NV-20: Provider stdout accepts unbounded protocol lines in the shared station process

- **Fingerprint:** `provider-stdout-unbounded-line`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** Both provider drivers read newline-delimited stdout with Tokio's unbounded lines adapter. A provider can withhold the newline while sending bytes, causing the station process to retain and grow the line buffer before parsing, logging, or any payload budget is applied. Exhaustion affects the shared station rather than only the child session.
- **Claimed root cause:** Provider stdout framing has no pre-allocation byte limit; post-parse raw-log payload limits and bounded event channels cannot constrain the String accumulated by next_line.

Source trace:

1. **entrypoint — DirectChildHost::spawn:** `crates/fleetdeck-providers/src/host/direct.rs:57` — Connects provider-controlled stdout to a pipe returned to the in-process driver.
2. **propagation — read_loop:** `crates/fleetdeck-providers/src/claude/driver.rs:458` — Wraps the provider stdout in BufReader::lines without a take/read_until byte limit.
3. **sink — read_loop:** `crates/fleetdeck-providers/src/claude/driver.rs:461` — next_line accumulates the complete line into a String before feed_line can parse, summarize, or reject it.

Verified source evidence:

- `crates/fleetdeck-providers/src/claude/driver.rs:492` — JSON parsing and raw-log handling occur only after next_line returns the complete String.
- `crates/fleetdeck-providers/src/codex/driver.rs:470` — The Codex driver independently uses the same unbounded lines pattern.
- `crates/fleetdeck-providers/src/rawlog.rs:120` — The 64 KiB diagnostic payload budget is applied only after a frame has already been read and encoded.
- `crates/fleetdeck-providers/src/adapter.rs:173` — The bounded event sink constrains downstream event count but not stdout line allocation.

Why it remains unconfirmed:

- Target execution was prohibited because the available platform cannot supply the complete required OS sandbox with all mandated network, filesystem, environment, process, resource, disk, and wall-time controls; the artifact promotion allowlist is empty.

Safe validation plan:

- **Local:** In a complete required OS sandbox, use a fake provider that writes one incrementally produced, non-secret stdout line slightly above a small proposed protocol-line cap and then waits. Under explicit low memory and wall limits, observe whether the current station continues buffering instead of terminating that session with a typed protocol error; stop after the first bounded observation.

### NV-21: Duplicate outstanding Codex IDs can make approval correlation ambiguous

- **Fingerprint:** `provider.codex-duplicate-native-id-approval-confusion`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** Codex server requests accept string or integer JSON-RPC IDs. Registration assigns each request a fresh app ID but overwrites by_native for a repeated native ID while leaving the earlier request open. Answering either app request emits only the stored native ID and selected decision. The host-side ambiguity is real, but no reviewed source establishes that the supported Codex dispatcher can produce duplicate outstanding IDs or which pending operation it would resolve; a malicious provider already controls its own action descriptions. Those external protocol facts are decisive.
- **Claimed root cause:** Codex request registration does not reject a duplicate outstanding native JSON-RPC ID, and replies do not carry a host-bound method, item, or action digest beyond that ID.

Source trace:

1. **entrypoint — rpc::parse_value:** `crates/fleetdeck-providers/src/codex/rpc.rs:60` — A provider request with any valid string or integer ID and method is accepted.
2. **propagation — CodexCore::register:** `crates/fleetdeck-providers/src/codex/requests.rs:205` — A fresh app ID is allocated while by_native.insert replaces an older mapping for the same native ID.
3. **propagation — CodexCore::answer:** `crates/fleetdeck-providers/src/codex/requests.rs:382` — The owner answer selects an app request and retrieves the native ID stored in that request without checking the reverse map.
4. **sink — CodexCore::answer RPC reply:** `crates/fleetdeck-providers/src/codex/requests.rs:445` — The emitted reply contains the duplicated native ID and decision but no action identity that distinguishes the two requests.

Verified source evidence:

- `crates/fleetdeck-providers/src/codex/rpc.rs:55` — valid_id accepts string and integer IDs without outstanding-request state.
- `crates/fleetdeck-providers/src/codex/requests.rs:209` — register unconditionally inserts the native key into by_native.
- `crates/fleetdeck-providers/src/codex/requests.rs:238` — Every registration remains independently open under its fresh app sequence key.
- `crates/fleetdeck-providers/src/claude/requests.rs:101` — The Claude sibling suppresses repeated native IDs, demonstrating an in-repository duplicate-handling baseline.

Why it remains unconfirmed:

- Target execution is prohibited because the host lacks the complete required sandbox, so no bounded protocol harness was run.
- The reviewed repository does not establish whether the supported Codex app-server ever permits duplicate outstanding IDs or how its dispatcher correlates such a reply; without that behavior there is no demonstrated unrequested action.

Safe validation plan:

- **Local:** In an approved sandbox, feed CodexCore two dummy approval requests with ID 7 and different harmless actions, answer only the first app request, and connect the emitted reply to the exact supported Codex app-server version instrumented to report which pending dummy request resolves. Stop at correlation; execute no action. A regression should reject the second outstanding duplicate before emitting an owner-facing approval.

### NV-22: Managed services persist unvalidated executable search paths

- **Fingerprint:** `service.persisted-path-executable-hijack`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** Service installation captures the invoking shell's PATH and writes it into the persistent systemd or launchd definition. Stations default to bare claude and codex binary names and the link runtime probes providers at start. If a persisted PATH component is writable by another local principal, that principal can place a matching executable that later runs as the Fleet Deck service owner.
- **Claimed root cause:** The installer treats ambient PATH as trusted persistent configuration without canonicalizing components or checking ownership, while default provider selections remain bare names resolved through that PATH.

Source trace:

1. **entrypoint — Context::detect:** `crates/fleetdeck-cli/src/service/mod.rs:93` — The installer copies the invoking process's PATH without component ownership or executable resolution.
2. **propagation — service::validate:** `crates/fleetdeck-cli/src/service/mod.rs:368` — PATH validation rejects control characters but does not require absolute owner-controlled components.
3. **propagation — systemd::render:** `crates/fleetdeck-cli/src/service/systemd.rs:113` — The accepted PATH is emitted into the persistent user service; launchd has the parallel EnvironmentVariables behavior.
4. **propagation — StationConfig::parse_with_store:** `crates/fleetdeck-core/src/config.rs:409` — Without explicit provider instances, configuration creates bare-name claude and codex programs.
5. **propagation — StationNode startup:** `crates/fleetdeck-station/src/link/runtime.rs:285` — The link runtime launches a provider-version probe task during station-node start.
6. **sink — ClaudeAdapter::run_probe:** `crates/fleetdeck-providers/src/claude/driver.rs:328` — Command::new executes the configured bare program through the persisted service PATH; Codex performs the same operation.

Verified source evidence:

- `crates/fleetdeck-cli/src/service/launchd.rs:89` — The launchd renderer serializes the validated environment into the agent definition.
- `crates/fleetdeck-core/src/config.rs:125` — Driver defaults are explicitly documented as binary names looked up on PATH.
- `crates/fleetdeck-providers/src/codex/driver_probe.rs:8` — The Codex probe also passes its configured program directly to Command::new.

Why it remains unconfirmed:

- Source does not reveal the effective PATH stored by a deployed service or the owner, mode, and ACL of each component; a lower-trust writable component is required.
- A deployed station may configure all provider programs as trusted absolute paths.
- The required sandbox is unavailable, so no harmless marker executable was selected through a synthetic service PATH.

Safe validation plan:

- **Local:** In a disposable VM or equivalent approved sandbox with dummy users, render a station service with a synthetic PATH whose first directory is writable by a lower-trust dummy account, place a harmless claude fixture that writes one scratch-local sentinel, start the user service under strict limits, and stop after observing whether startup creates the sentinel. Repeat with a trusted absolute provider path.
- **Owner-observed deployment check:** Inspect each deployed systemd Environment or launchd EnvironmentVariables value, record owner/mode/ACL for every PATH component, and verify configured provider/helper programs are trusted absolute files. Do not place or execute test binaries on production paths.

### NV-23: Provider-reported Codex session IDs can be reinterpreted as TUI options

- **Fingerprint:** `shell.handover-native-id-argv-option-confusion`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** The station stores the native thread ID supplied in a provider session.started event without applying the native-session-ID validation used by session.import. A later terminal handover appends that stored value as Codex's final positional resume argument without an option terminator. If a Codex-compatible child can report a leading-hyphen ID and the installed Codex CLI parses it as an option, the resumed TUI can run with an option not selected by the owner, potentially changing its sandbox or approval behavior.
- **Claimed root cause:** Provider-reported native resume identities are persisted and reused in a command line without the repository's existing check_native_session_id invariant, and the Codex resume argv does not place `--` before the attacker-influenced positional value.

Source trace:

1. **entrypoint — append_one session.started projection:** `crates/fleetdeck-station/src/store/log.rs:166` — The station accepts provider_refs.session_id or provider_refs.thread_id from the provider event and writes the string directly to sessions.native_session_id.
2. **propagation — Store::handover_launch:** `crates/fleetdeck-station/src/store/terminal.rs:80` — Terminal handover reloads native_session_id from SQLite into HandoverLaunch without validating its length, alphabet, or leading character.
3. **propagation — tui_command Codex branch:** `crates/fleetdeck-station/src/shell/handover.rs:103` — The builder appends the native value after Codex resume options as a positional argv element but emits no `--` terminator.
4. **sink — Shells::spawn:** `crates/fleetdeck-station/src/shell/mod.rs:525` — CommandBuilder executes the provider binary with the constructed argv in the session workspace.

Verified source evidence:

- `crates/fleetdeck-proto/src/command.rs:347` — The sibling session.import boundary explicitly requires an alphanumeric first character so a native ID can never read as a CLI flag.
- `crates/fleetdeck-station/src/store/log.rs:166` — The provider-event projection stores a native ID without calling check_native_session_id or an equivalent validator.
- `crates/fleetdeck-station/src/store/terminal.rs:85` — handover_launch returns the stored native string unchanged.
- `crates/fleetdeck-station/src/shell/handover.rs:116` — Codex's native ID is pushed directly as the last argument, with no option terminator or validation.

Why it remains unconfirmed:

- This run is source-only because the host cannot provide the required cumulative disk quota and isolated loopback sandbox controls, so the provider TUI parser was not executed.
- The reviewed repository does not establish whether the installed Codex CLI accepts a leading-hyphen resume positional as an option or whether a model/provider path can produce such a native ID without already holding equivalent process authority.

Safe validation plan:

- **Local:** In an approved no-network, read-only-target, scratch-only sandbox with strict CPU, memory, process, file-size, cumulative-disk, and wall-clock limits, first exercise the installed pinned Codex CLI parser with a dummy resume ID beginning with a harmless diagnostic flag and record whether it is parsed as an option rather than an ID. Then run the handover path against a bounded fake provider fixture that emits that ID and capture only argv and exit status. The regression should reject provider IDs failing check_native_session_id and place an option terminator before any supported positional ID.

### NV-24: Terminal handover does not revalidate current workspace or bypass-mode policy

- **Fingerprint:** `shell.handover-stale-launch-policy`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** Handover admission applies resume checks, including current allowed-root validation, but the effect can dispatch under a later policy snapshot. Dispatch checks current allow_shell and max_shells, then reloads stored cwd and mode and starts the provider TUI without rerunning policy.workspace. TerminalInstance also carries no allow_bypass_permissions bit, so a stored recognized bypass/full-access mode is translated directly after that setting is disabled. Bounded process-spawn evidence is required to confirm both stale-policy effects.
- **Claimed root cause:** The final handover launch treats persisted cwd and mode as already authorized; unlike shell.open and normal adapter construction, it does not reapply current workspace or bypass-mode restrictions at the last trusted decision point.

Source trace:

1. **entrypoint — handover::admit:** `crates/fleetdeck-station/src/shell/handover.rs:148` — A handover is admitted after resumable validates the session under the policy snapshot at command decision time.
2. **propagation — shell::dispatch:** `crates/fleetdeck-station/src/shell/dispatch.rs:82` — The queued effect is later claimed and a fresh policy snapshot is loaded.
3. **propagation — start_terminal:** `crates/fleetdeck-station/src/shell/dispatch.rs:148` — The final gate checks allow_shell and max_shells, then loads persisted launch facts without policy.workspace or bypass-mode validation.
4. **propagation — tui_command:** `crates/fleetdeck-station/src/shell/handover.rs:87` — Recognized stored modes, including bypass/full-access modes, are converted directly into provider CLI arguments.
5. **sink — Shells::spawn:** `crates/fleetdeck-station/src/shell/mod.rs:525` — The TUI is spawned with the persisted cwd and generated mode arguments.

Verified source evidence:

- `crates/fleetdeck-station/src/manager/resume.rs:138` — Admission canonicalizes the stored cwd and enforces policy.workspace.
- `crates/fleetdeck-station/src/shell/mod.rs:293` — The sibling queued shell.open path rechecks allow_shell and policy.workspace immediately before spawn.
- `crates/fleetdeck-station/src/manager/mod.rs:93` — Normal provider adapters receive allow_bypass_permissions through allow_bypass or allow_full_access.
- `crates/fleetdeck-station/src/serve.rs:344` — TerminalInstance contains only driver, binary, and environment, with no bypass-policy field.
- `crates/fleetdeck-station/src/shell/handover.rs:531` — A unit test expects a stored Codex never/danger-full-access mode to pass through unchanged.

Why it remains unconfirmed:

- Target execution is prohibited because the required sandbox controls, including cumulative disk quota, are unavailable; no fake TUI process was started.
- A bounded observed result is needed to show a queued handover starts in a removed dummy root and a stored bypass/full-access mode starts after bypass is disabled.

Safe validation plan:

- **Local:** In an approved no-network sandbox, use temporary allowed roots and a fake provider that records only cwd and argv. Enqueue under root A, publish a policy allowing only root B, then dispatch. Separately persist an ended dummy session with a bypass/full-access mode, restart with allow_bypass_permissions=false, and request handover. A fixed path must refuse before spawn or intentionally downgrade to the safest allowed mode.

### NV-25: Drain can report idle before a prechecked command commits new work

- **Fingerprint:** `station.drain-prechecked-submit-after-idle`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** An authorized collaborator can begin a work-creating command immediately before an operator starts upgrade drain. submit reads the drain flag before queuing its database decision; drain can then set the flag, observe an idle database snapshot, and return before the earlier decision commits. The accepted post-idle effect can be retired or become outcome-unknown when the operator stops the station.
- **Claimed root cause:** Drain admission is a check-then-act across an atomic flag and a later serialized database decision. The drain snapshot does not count submissions that already passed the flag check but have not yet entered the database queue.

Source trace:

1. **entrypoint — member_command:** `crates/fleetdeck-deck/src/http/command_routes.rs:290` — A collaborator may send Prompt, Steer, or FollowUp to an authorized shared session concurrently with the local operator.
2. **propagation — SessionManager::submit:** `crates/fleetdeck-station/src/manager/mod.rs:577` — submit computes and caches drain::guard before awaiting Store::decide.
3. **propagation — SessionManager::drain:** `crates/fleetdeck-station/src/manager/drain.rs:87` — Drain sets a boolean and returns idle from a persisted-session snapshot without fencing already prechecked submissions.
4. **propagation — Db::call:** `crates/fleetdeck-core/src/db.rs:169` — Database work is ordered only when jobs are submitted, leaving the earlier flag read outside serialization.
5. **sink — recover_effects:** `crates/fleetdeck-station/src/store/effects.rs:753` — Restart retires qualifying queued effects and converts claimed effects to outcome-unknown after the operator acts on the idle report.

Verified source evidence:

- `crates/fleetdeck-station/src/manager/mod.rs:586` — The drain guard result is moved into the later database decision closure and is not recomputed there.
- `crates/fleetdeck-station/src/manager/restart_gate.rs:124` — The separate restart gate counts session-creating submissions before reading its flag, demonstrating a synchronization pattern absent from drain.
- `crates/fleetdeck-station/src/manager/drain.rs:100` — An empty busy-session list is sufficient for drain to return idle immediately.

Why it remains unconfirmed:

- The required sandbox is unavailable, so the decisive interleaving and its post-idle receipt/effect state were not reproduced with a deterministic test barrier.

Safe validation plan:

- **Local:** Inside the required sandbox, add a test-only barrier after submit computes a successful drain guard and before Store::decide is queued. Pause one dummy collaborator Prompt, call drain and assert idle, release the Prompt, verify an accepted receipt and queued effect appear after the idle report, then run bounded recovery and verify the effect is retired. A regression must make drain wait for or transactionally reject every prechecked work-creating submission.

### NV-26: Production station never runs its configured event and receipt retention

- **Fingerprint:** `station.event-receipt-prune-unscheduled`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** A provider process can keep producing station events and authenticated callers can keep producing stored command receipts. Although the store defines seven-day event retention and protocol receipt retention and implements acknowledgment-aware deletion, the production station never invokes that implementation. The station database can therefore retain every event and stored receipt for the life of the installation, consuming the operator's shared database and disk capacity beyond the configured windows.
- **Claimed root cause:** Store::prune is an opt-in API with no production caller or scheduled maintenance task. RunningStation::start opens Store with StoreConfig::default and starts the manager, link/local servers, credential rescan, and assist watcher, but neither store startup nor any long-lived production loop invokes pruning; only tests call the API.

Source trace:

1. **entrypoint — provider event pump:** `crates/fleetdeck-station/src/manager/target.rs:704` — The station continuously receives batches of events emitted by a provider process and passes every batch to the store.
2. **propagation — Store::append:** `crates/fleetdeck-station/src/store/mod.rs:390` — Each provider or station event is appended transactionally without applying retention before or after the append.
3. **sink — log::append_at:** `crates/fleetdeck-station/src/store/log.rs:138` — Every accepted event becomes a persistent events row in station.db; the only deletion routine is the separately callable prune function.

Verified source evidence:

- `crates/fleetdeck-station/src/store/mod.rs:131` — StoreConfig exposes event_retention_ms and receipt_retention_ms, and the default applies the documented retention constants.
- `crates/fleetdeck-station/src/store/log.rs:295` — The implemented pruner transactionally deletes only an acknowledged, aged event prefix and receipts older than their configured window.
- `crates/fleetdeck-station/src/store/mod.rs:489` — Retention is exposed only as an explicit async Store::prune call; store open/start does not invoke it.
- `crates/fleetdeck-station/src/serve.rs:170` — Production startup opens the database with StoreConfig::default.
- `crates/fleetdeck-station/src/serve.rs:229` — The production station constructs its long-lived tasks and servers without a retention or prune task.
- `crates/fleetdeck-station/src/store/receipts.rs:323` — Every stored accepted or storable rejected command inserts a commands receipt row.
- `crates/fleetdeck-station/src/store/tests.rs:201` — Retention tests invoke Store::prune directly, showing the API works only when explicitly called; no equivalent production call exists.

Why it remains unconfirmed:

- Target execution was prohibited because the required OS-enforced sandbox controls were unavailable: empty allowlisted environment, disabled external networking, read-only target and toolchain, scratch-only writes, and explicit CPU, memory, process, file-size, disk, and wall-clock limits. The empty artifact promotion allowlist also precluded retained observed evidence.

Safe validation plan:

- **Local:** Inside the required sandbox, run a bounded store/startup harness with a fake clock and a scratch-only SQLite database: configure millisecond event and receipt windows, create one dummy session, append a small fixed set of events and stored commands, advance the fake clock, and drive the same production maintenance lifecycle. Verify that counts stay unchanged without an explicit prune call, then call Store::prune once and verify that eligible rows drop. After a fix, repeat and verify the production retention task performs the same bounded deletion while preserving an unacknowledged event.

### NV-27: Hibernated input can become an uncounted resume during restart

- **Fingerprint:** `station.restart-hibernated-input-resume-bypass`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** RestartState counts and rejects only raw Start, Import, and Resume commands. Prompt, Steer, and FollowUp are uncounted even though decision logic transforms them into Resume effects for exited hibernated sessions. After restart admission has listed and retired existing starts and committed its freeze, one of these inputs can be accepted during the fixed pre-restart delay, remain undispatched, and then be retired by startup recovery. This can sacrifice collaborator work outside the elevated owner's consent snapshot, but the schedule has not been reproduced.
- **Claimed root cause:** Restart gating classifies the raw SessionCommand before state-aware normalization, while resume_hibernated later turns otherwise uncounted input commands into process-starting Resume effects.

Source trace:

1. **entrypoint — command_routes::member_command:** `crates/fleetdeck-deck/src/http/command_routes.rs:294` — A covering collaborator may submit Prompt, Steer, or FollowUp to the shared session.
2. **propagation — creates_session and guard:** `crates/fleetdeck-station/src/manager/restart_gate.rs:165` — Restart accounting and rejection recognize only raw Start, Import, and Resume.
3. **propagation — decide input branches:** `crates/fleetdeck-station/src/manager/decide.rs:146` — Prompt, Steer, and FollowUp call resume_hibernated before ordinary live-session handling.
4. **propagation — resume_hibernated:** `crates/fleetdeck-station/src/manager/decide.rs:704` — For an exited hibernated direct session, the input is admitted as a Resume effect.
5. **propagation — StationSettings::restart:** `crates/fleetdeck-station/src/settings_svc.rs:283` — Restart retires queued starts once, commits the freeze, and schedules process restart after a delay.
6. **sink — recover_effects:** `crates/fleetdeck-station/src/store/effects.rs:753` — Startup recovery retires queued process-starting effects that did not dispatch.

Verified source evidence:

- `crates/fleetdeck-station/src/manager/restart_gate.rs:3` — The documented invariant is that an accepted restart must not stop a session that starts after the check.
- `crates/fleetdeck-station/src/manager/mod.rs:584` — The submit counter is entered solely when creates_session recognizes the original command.
- `crates/fleetdeck-station/src/manager/hibernated_input_tests.rs:63` — Existing tests prove each hibernated input variant produces Resume state but do not combine it with restart.
- `crates/fleetdeck-deck/src/store/members.rs:109` — Prompt, Steer, and FollowUp require Collaborator, below the elevated owner who approves restart.
- `crates/fleetdeck-station/src/settings_svc.rs:290` — The restart request is made asynchronously after RESTART_DELAY, leaving a source-visible post-commit interval.

Why it remains unconfirmed:

- Target execution is prohibited because the complete required sandbox is unavailable, so the cross-gate schedule and accepted-then-retired receipt were not observed.

Safe validation plan:

- **Local:** In an approved scratch-only deterministic fixture, create one exited hibernated direct-host session shared to a dummy collaborator. Pause restart immediately after fail_queued_starts, submit one Prompt, and verify whether it receives an accepted receipt and queues Resume while the committed freeze holds. Run bounded startup recovery and inspect only the effect state and dummy receipt. The regression must reject state-dependent resumes once restart admission begins.

### NV-28: Terminal station recovery records have no retention lifecycle

- **Fingerprint:** `station.terminal-recovery-state-unbounded-retention`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** Authorized session activity creates durable provider effects, processes, runtime requests, checkpoints, shim records, and shell effects. Completion changes these rows to terminal states, while restart reads active subsets and the general pruner removes only events and command receipts. Repeated activity can therefore grow shared station.db and its indexes indefinitely; the availability threshold remains unmeasured.
- **Claimed root cause:** Recovery tables preserve terminal state for idempotency and diagnosis but have no age-, count-, byte-, or session-lifecycle compaction policy, and session rows have no deletion lifecycle that could remove their dependents.

Source trace:

1. **entrypoint — accepted session command:** `crates/fleetdeck-station/src/manager/decide.rs:581` — An accepted command creates a durable provider effect as part of its receipt transaction.
2. **propagation — effects::enqueue:** `crates/fleetdeck-station/src/store/effects.rs:326` — The decision inserts complete durable effect state.
3. **propagation — effects::transition:** `crates/fleetdeck-station/src/store/effects.rs:544` — Completion and failure update the same row to a terminal state rather than deleting it.
4. **sink — log::prune:** `crates/fleetdeck-station/src/store/log.rs:295` — The general retention pass deletes events and commands only, leaving recovery history untouched.

Verified source evidence:

- `crates/fleetdeck-station/src/store/schema.rs:92` — processes, effects, and runtime_requests hold per-activity rows with active-state indexes but no retention metadata or cascading parent deletion.
- `crates/fleetdeck-station/src/store/recovery.rs:79` — Restart recovery selects live processes and active effects/requests while terminal rows remain stored.
- `crates/fleetdeck-station/src/store/turn_checkpoints.rs:267` — Each application/provider turn creates a durable checkpoint row; settled-state queries retain it.
- `crates/fleetdeck-station/src/store/shims.rs:514` — Shim end changes an open row to a terminal state rather than deleting it.
- `crates/fleetdeck-station/src/store/shell_effects.rs:199` — Shell effects finish by changing state to confirmed or failed and remain durable.

Why it remains unconfirmed:

- The required sandbox is unavailable, so bounded repeated lifecycle cycles could not establish retained row/byte growth after recovery and pruning.
- Deployment database quotas, maintenance, session volume, supervisor isolation, and the point at which retained history affects availability are unknown.

Safe validation plan:

- **Local:** Inside the required sandbox, use a scratch-only SQLite store and a small fixed number of dummy session cycles. In each cycle settle one provider effect/process/request/checkpoint and one shell or shim record, run recovery and the existing pruner, and record table counts and database bytes while active recovery results remain empty. Validate a dependency-safe retention policy that preserves open work and required idempotency windows.
- **Owner-observed deployment check:** The owner should inspect station.db growth, table cardinalities, filesystem quota/alerts, backup expectations, and any external SQLite maintenance policy without generating additional workload.

### NV-29: Failed live-upgrade marker can let rollback restore revoked station authority

- **Fingerprint:** `station.upgrade-live-marker-fail-open-rollback`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** After an activated station build starts, a malformed sibling update record or an update-record write failure can make the live-advanced marker fail. Startup logs the error and continues to open and use the live database. If an owner then successfully revokes a paired deck and later rolls back while no shim is live, rollback still considers the database unadvanced and restores the pre-activation snapshot. That snapshot can contain the revoked pairing and grants, which station startup loads and pins again, reviving the lower-trust deck's authority.
- **Claimed root cause:** RunningStation::start deliberately continues after upgrade::mark_live fails, but rollback_plan later uses the durable live_advanced bit as its principal proof that restoring a historical snapshot is safe. records() makes this marker dependent on every JSON file in the upgrades directory, and the restore path replaces the live database and its WAL/SHM sidecars without reconciling post-snapshot revocations.

Source trace:

1. **entrypoint — RunningStation::start:** `crates/fleetdeck-station/src/serve.rs:152` — The activated station enters production startup and must durably mark that the live database is about to advance before any later rollback may restore an old snapshot.
2. **propagation — records and mark_live:** `crates/fleetdeck-station/src/upgrade.rs:131` — mark_live first parses every .json update record and saves live_advanced=true for activated records; one malformed sibling record or save failure returns an error while leaving the target record false.
3. **propagation — RunningStation::start marker error handling:** `crates/fleetdeck-station/src/serve.rs:165` — The marker error is reduced to a warning, after which the station opens station.db and admits normal work despite the rollback-safety proof remaining false.
4. **propagation — Store::revoke_peer:** `crates/fleetdeck-station/src/store/peers.rs:186` — A subsequent owner revocation can transactionally delete a paired deck and advance the revocation generation in the live database.
5. **propagation — station upgrade rollback:** `crates/fleetdeck-cli/src/upgrade.rs:146` — After the station stops, rollback reloads the still-false update record and chooses the rollback plan using only that flag and the count of live or unresolved shims.
6. **propagation — rollback_plan and restore:** `crates/fleetdeck-station/src/upgrade.rs:267` — With live_advanced false and no live shims, the plan permits RestoreSnapshot, whose implementation copies the pre-activation database, WAL, and SHM over the live bundle.
7. **sink — StationNode startup peer load:** `crates/fleetdeck-station/src/link/runtime.rs:238` — The next station startup loads paired keys and grants from the restored database and pins them into the live link node, so the snapshot's formerly revoked deck becomes trusted again.

Verified source evidence:

- `crates/fleetdeck-station/src/serve.rs:163` — The comment defines the security invariant that live database advancement begins here and rollback must become binary-only, but the adjacent error branch only warns when marking fails.
- `crates/fleetdeck-station/src/upgrade.rs:127` — records() walks every JSON file and aborts on the first parse/read error, so an unrelated malformed record can prevent all activated records from being marked.
- `crates/fleetdeck-station/src/upgrade.rs:150` — mark_live is the only transition that persists live_advanced=true before database open.
- `crates/fleetdeck-station/src/upgrade.rs:264` — rollback_plan restores the snapshot whenever live_advanced is false and no shim is live or unresolved; it has no independent proof that live state was used.
- `crates/fleetdeck-cli/src/upgrade.rs:154` — The production rollback command applies that plan and invokes restore on the recorded pre-activation snapshot.
- `crates/fleetdeck-station/src/upgrade.rs:302` — restore copies the snapshot's main database and SQLite sidecars over the live paths, then changes only the database incarnation.
- `crates/fleetdeck-station/src/store/peers.rs:180` — A legitimate peer revocation deletes the pairing and advances the revocation generation in one transaction, establishing a concrete post-snapshot security state that restore can erase.
- `crates/fleetdeck-station/src/store/peers.rs:215` — Station startup's paired-key query returns every fingerprinted pairing with its role and scopes from durable state.
- `crates/fleetdeck-station/src/link/runtime.rs:239` — StationNode loads the persistent peer set and pin_all installs it into the node before serving links.

Why it remains unconfirmed:

- Target execution was prohibited because the required OS-enforced sandbox controls were unavailable: empty allowlisted environment, disabled external networking, read-only target and toolchain, scratch-only writes, and explicit CPU, memory, process, file-size, disk, and wall-clock limits. The empty artifact promotion allowlist also precluded retained observed evidence.

Safe validation plan:

- **Local:** Inside the required sandbox, create a scratch-only station database and update directory with dummy identities. Pair one dummy deck and snapshot the main/WAL/SHM bundle; save an Activated update record with live_advanced=false plus one malformed unrelated .json record. Invoke the production startup marker and verify it errors while store open can proceed. Revoke the dummy deck through Store::revoke_peer and verify paired_keys no longer returns it. Stop all handles, assert live_shims is zero, apply rollback_plan and restore, then reopen the store and StationPeers and verify the old deck and grants reappear. The regression must make startup fail closed until the marker is durable, or otherwise make any marker failure permanently prohibit snapshot restore, and must preserve the committed revocation. Use a fixed one-peer fixture, no external network, low explicit limits, and terminate at the first restored-authority observation.

### NV-30: Workspace preflight buffers complete Git remote output

- **Fingerprint:** `workspace.preflight.unbounded-git-output`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** A Drive-scoped caller can select an accepted HTTPS, SSH, or scp-like Git remote for preflight. The station executes git ls-remote with a 20-second timeout, but Git::status uses wait_with_output and collects complete stdout and stderr before successful stdout is discarded and stderr is truncated. A caller-selected remote can therefore drive station-process allocation during the deadline. Link-wide outstanding-request limits bound concurrency but not output bytes.
- **Claimed root cause:** Preflight uses the generic unbounded Git::status collector instead of Git::bounded or another streaming per-stream cap for remote-controlled ls-remote output.

Source trace:

1. **entrypoint — workspace_routes::preflight:** `crates/fleetdeck-deck/src/http/workspace_routes.rs:118` — A Drive-authorized caller supplies the clone URL to preflight.
2. **propagation — intake::preflight:** `crates/fleetdeck-station/src/link/intake.rs:145` — The station applies clone URL syntax/policy and invokes workspace preflight.
3. **propagation — preflight::run:** `crates/fleetdeck-station/src/workspace/preflight.rs:49` — Preflight invokes Git::status for ls-remote --heads on the caller-selected remote.
4. **sink — Git::status:** `crates/fleetdeck-station/src/workspace/git.rs:231` — wait_with_output accumulates both pipes before stderr truncation or successful stdout discard.

Verified source evidence:

- `crates/fleetdeck-station/src/workspace/preflight.rs:45` — Preflight sets a 20-second process deadline but no output-byte limit.
- `crates/fleetdeck-station/src/workspace/provision.rs:234` — URL policy accepts attacker-controlled HTTPS, SSH, and scp-like remotes after credential and syntax checks.
- `crates/fleetdeck-station/src/workspace/git.rs:239` — stderr is shortened only after complete child output has been captured.
- `crates/fleetdeck-station/src/workspace/git.rs:255` — Git::bounded provides an existing streaming max-bytes pattern that preflight does not use.
- `crates/fleetdeck-link/src/control.rs:45` — One link permits 128 outstanding requests, an aggregate request cap rather than a preflight memory budget.

Why it remains unconfirmed:

- Target execution is prohibited because the host lacks cumulative disk quota and usable isolated loopback as part of the required sandbox, so no fake-Git memory fixture ran.
- The deployed Git version's exact output behavior and station memory/supervisor limits are outside repository source.

Safe validation plan:

- **Local:** In an approved no-network sandbox with strict memory, process, output, cumulative-disk and time limits, configure the Git program as a fixture executable that emits several small increasing stdout/stderr sizes before exit. Issue one dummy preflight and record peak RSS and response behavior, stopping at the first linear allocation point. A fixed regression must use a streaming cap.
- **Owner-observed deployment check:** The owner should document station memory limits and restart scope and identify the exact deployed Git version; do not contact an external remote or perform a production load test.

### NV-31: Missing HOME can omit provider credential files from workspace privacy policy

- **Fingerprint:** `workspace.private-path-home-unset-disclosure`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** The station builds home credential protections from HOME and continues with those entries omitted when HOME is absent or invalid. With an explicit allowed root containing the actual user home, a Drive bearer can select that workspace and request .claude/.credentials.json or .codex/auth.json; those directory names are not globally hidden, so the descriptor-safe reader can return credential text.
- **Claimed root cause:** PrivatePaths fails open for home credential locations when HOME is unavailable, retaining a usable partial deny set instead of denying projections whose roots may contain an unknown home.

Source trace:

1. **entrypoint — file_routes::read:** `crates/fleetdeck-deck/src/http/file_routes.rs:249` — A Drive bearer may submit a workspace root and relative path; only member principals are forced onto a shared-session root.
2. **propagation — StationServer startup:** `crates/fleetdeck-station/src/serve.rs:187` — PrivatePaths is constructed from Env::from_process().home.
3. **propagation — PrivatePaths::new:** `crates/fleetdeck-station/src/workspace/private_paths.rs:154` — When home is None, gh, glab, SSH, Claude, and Codex home credential entries are omitted rather than projections being denied.
4. **propagation — manager::files::root:** `crates/fleetdeck-station/src/manager/files.rs:49` — A caller-selected directory is accepted if it canonicalizes under an allowed root and is absent from the partial private set.
5. **sink — files::read_for:** `crates/fleetdeck-station/src/workspace/files.rs:356` — After policy checks, the regular file is opened without following symlinks and up to the bounded content limit is returned.

Verified source evidence:

- `crates/fleetdeck-core/src/paths.rs:29` — HOME is discarded when unset, empty, or relative.
- `crates/fleetdeck-station/src/workspace/private_paths_tests.rs:141` — A source regression verifies unknown HOME leaves .codex/auth.json non-private while station-owned secrets remain private.
- `crates/fleetdeck-proto/src/files.rs:361` — The global hidden names include .ssh but not .claude or .codex.
- `crates/fleetdeck-core/src/config.rs:366` — Absolute allowed roots remain valid without HOME; only tilde expansion requires it.
- `crates/fleetdeck-deck/src/http/methods_table.rs:237` — Direct file reads are authorized to Drive scope.

Why it remains unconfirmed:

- Source does not reveal whether a station runs without an absolute HOME while an explicit allowed root contains its actual user home.
- Source does not reveal whether the omitted credential files exist beneath such a root and are readable by the station identity.
- The required sandbox is unavailable, so a harmless sentinel was not observed through the complete route.

Safe validation plan:

- **Local:** In the required sandbox, start a dummy station fixture with HOME removed, an explicit temporary allowed root, and harmless sentinel files at .claude/.credentials.json and .codex/auth.json. Invoke the file service as a dummy Drive principal and record whether either sentinel is returned; then verify deny-all-on-unknown-home or explicit private roots block it.
- **Owner-observed deployment check:** Record effective HOME and allowed_roots plus existence and metadata of the named credential paths without printing contents. The candidate applies only where HOME is unavailable or invalid and an allowed root contains an omitted credential path.

### NV-32: Checkpoint restore can follow an ancestor swapped after validation

- **Fingerprint:** `workspace.restore.path-check-use-race`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** A lower-trust process that can mutate the workspace may replace a checked directory with a symlink after plan_restore returns. apply_restore subsequently uses ordinary pathnames for deletion and Git checkout, potentially causing the station's filesystem authority to delete or overwrite a same-UID path outside the workspace.
- **Claimed root cause:** Restore separates pathname-based ancestor validation from pathname-based mutation and does not reopen or revalidate components with descriptor-relative no-follow semantics at the mutation sinks.

Source trace:

1. **entrypoint — Worktree::plan_restore:** `crates/fleetdeck-station/src/workspace/restore.rs:197` — The station plans a restore from the current provider-controlled workspace filesystem.
2. **propagation — Worktree::plan_restore:** `crates/fleetdeck-station/src/workspace/restore.rs:250` — Each removal path is accepted after a separate ancestor check.
3. **propagation — Worktree::check_ancestors:** `crates/fleetdeck-station/src/workspace/restore.rs:423` — Ancestor safety is checked by pathname with symlink_metadata and no stable directory handle is retained.
4. **propagation — restore_files:** `crates/fleetdeck-station/src/manager/revert_run.rs:264` — The manager awaits plan_restore and later passes the reusable plan to apply_restore.
5. **sink — Worktree::apply_restore:** `crates/fleetdeck-station/src/workspace/restore.rs:295` — remove_file is called on root.join(path), allowing normal ancestor resolution after the validation gap.

Verified source evidence:

- `crates/fleetdeck-station/src/workspace/restore.rs:4` — The module explicitly intends checks to precede mutation and says changed paths must not lie below a symlink.
- `crates/fleetdeck-station/src/workspace/restore.rs:280` — apply_restore trusts the prior plan and describes concurrent change only as something reported later by matches.
- `crates/fleetdeck-station/src/workspace/restore.rs:312` — Write paths are also passed by name to checkout-index without component revalidation.
- `crates/fleetdeck-station/src/manager/revert_run.rs:279` — The production restore path calls apply_restore after the separately awaited planning and tree comparison steps.

Why it remains unconfirmed:

- Target execution is prohibited because the required empty-environment, network-isolated, read-only-target, scratch-only, resource-limited OS sandbox is unavailable.
- The promotion allowlist is empty, so no target-controlled local proof artifact can be promoted.

Safe validation plan:

- **Local:** Inside the required sandbox, create a disposable Git worktree and a dummy outside directory. Add a test barrier after plan_restore, atomically replace a checked workspace ancestor with a symlink to the dummy directory, then call apply_restore for one removal and one write. Record whether only the workspace changes or the dummy marker is deleted/overwritten; use no real user paths and strict time/resource limits.

### NV-33: Review anchor cache can outlive a stricter member file policy

- **Fingerprint:** `workspace.review-anchor.member-deny-cache-stale`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** Member-filtered parsed diffs are cached for 60 seconds by station, session, snapshot, and only a member boolean. Member-deny lists are cached independently for 60 seconds by station. A successful station settings change publishes a new station Policy but does not invalidate either deck cache. With staggered cache ages, the deny cache can refresh to the stricter policy while an older parsed diff remains live; anchor_of then accepts protected lines from the cached diff without applying the current Hide policy, persists them as anchor_text, and returns them to the collaborator.
- **Claimed root cause:** Review caches are authorized only when filled and carry no station policy generation or production invalidation; a successful cached diff anchor bypasses the current deny policy before persistence and response.

Source trace:

1. **entrypoint — review_routes::add:** `crates/fleetdeck-deck/src/http/review_routes.rs:313` — A covering collaborator submits a top-level comment naming a snapshot, path, and line range.
2. **propagation — review diff cache Key:** `crates/fleetdeck-deck/src/http/review_anchor_cache.rs:82` — Parsed diffs are keyed without member_deny contents or station policy version.
3. **propagation — settings_routes::station_set:** `crates/fleetdeck-deck/src/http/settings_routes.rs:493` — A successful station settings update has no production cache-invalidation call.
4. **propagation — StationSettings::set:** `crates/fleetdeck-station/src/settings_svc.rs:149` — The station saves and immediately publishes the new Policy snapshot.
5. **propagation — review_routes::anchor_of:** `crates/fleetdeck-deck/src/http/review_routes.rs:271` — A successful cached diff anchor is accepted without consulting the current Hide argument.
6. **propagation — DeckStore::review_add:** `crates/fleetdeck-deck/src/store/review.rs:658` — The anchor path and text are inserted without another deny check.
7. **sink — review_routes::add response:** `crates/fleetdeck-deck/src/http/review_routes.rs:365` — The stored ReviewComment, including anchor_text, is returned directly to the collaborator.

Verified source evidence:

- `crates/fleetdeck-deck/src/http/review_anchor_cache.rs:35` — Both diff and deny caches use a fixed 60-second TTL.
- `crates/fleetdeck-deck/src/http/review_anchor_cache.rs:131` — clear_station can invalidate both caches but is hidden for tests and has no production caller.
- `crates/fleetdeck-deck/src/http/review_anchor_cache.rs:336` — deny_list fails closed on refresh failure, but a live cache entry returns before refresh.
- `crates/fleetdeck-station/src/manager/policy_hub.rs:92` — The station already increments a policy version on every published snapshot.
- `crates/fleetdeck-deck/src/http/review_routes.rs:286` — Only the imported-note fallback applies Hide; the successful diff-anchor branch does not.
- `crates/fleetdeck-proto/src/review.rs:130` — ReviewComment serializes anchor_text to the API caller.

Why it remains unconfirmed:

- Target execution is prohibited because the complete required sandbox is unavailable, so no fake-clock dummy-member fixture observed the stale protected anchor disclosure.
- The exact exploitable interval depends on independently staggered cache insertion times, although source establishes the missing invalidation and absent retrieval-time check.

Safe validation plan:

- **Local:** In an approved isolated fixture with a fake clock, harmless sentinel file, dummy station and two collaborators, fill the deny cache at time 0 and member diff cache at time 30, apply a stricter member_deny at time 31, and at time 61 verify an uncached projection enforces the new rule while the old diff remains live. Have the second collaborator add a top-level comment for the sentinel and record only whether the response contains anchor_text and whether a row is stored.

### NV-34: Read-scoped session diffs can repeatedly materialize large workspace snapshots

- **Fingerprint:** `workspace.session-diff-unthrottled-materialization`
- **Disposition:** Needs validation — this is not assigned a severity and is not a confirmed vulnerability.
- **Boundary and possible result:** A Read-scoped caller can trigger session-diff snapshot creation with a small request. One request may inspect up to 100,000 tracked files and 512 MiB, read changed contents, write blobs with git hash-object -w, and write a tree. Unlike normal reads and searches, this path takes no operation-specific slot; up to 128 link requests can be open. Timeouts and per-snapshot limits constrain each call but not aggregate concurrent work or persistent Git objects.
- **Claimed root cause:** The read-only diff projection reuses a snapshot implementation that writes to the repository object database, but it has no diff-specific concurrency/rate budget and no projection-object cleanup policy.

Source trace:

1. **entrypoint — file_routes::session_diff:** `crates/fleetdeck-deck/src/http/file_routes.rs:334` — A caller sends a small session identifier and optional response byte limit.
2. **propagation — files.session_diff method:** `crates/fleetdeck-deck/src/http/methods_table.rs:314` — The route requires only Read scope.
3. **propagation — ordinary request dispatch:** `crates/fleetdeck-station/src/link/mod.rs:141` — Each ordinary inbound request is spawned as a separate task.
4. **propagation — session_diff computation:** `crates/fleetdeck-station/src/workspace/session_diff.rs:148` — The route enters snapshot computation under a 15-second timeout without taking a file-read or search slot.
5. **propagation — Worktree::snapshot_tree:** `crates/fleetdeck-station/src/workspace/checkpoint.rs:401` — The projection stages changes through a private index and writes a Git tree object.
6. **sink — Worktree::stage_changes_with:** `crates/fleetdeck-station/src/workspace/stage.rs:339` — Every changed regular file is read and passed to git hash-object -w, writing blobs into the repository object database.

Verified source evidence:

- `crates/fleetdeck-station/src/workspace/checkpoint.rs:132` — One snapshot permits 50 MiB per file, 512 MiB total, and 100,000 tracked files.
- `crates/fleetdeck-station/src/manager/files.rs:91` — Ordinary file workers use explicit slots; session_diff does not acquire one.
- `crates/fleetdeck-link/src/control.rs:44` — The broad link-wide cap permits 128 outstanding requests.
- `crates/fleetdeck-station/src/workspace/checkpoint.rs:581` — Dropping the private index removes only index files, not blobs or tree objects.
- `crates/fleetdeck-station/src/workspace/git.rs:44` — Fleet Deck disables automatic Git maintenance and garbage collection for its calls.

Why it remains unconfirmed:

- The required sandbox is unavailable, so CPU, RSS, Git child concurrency, cancellation, and object-database growth were not measured with bounded synthetic worktrees.
- Deployment CPU/memory limits, filesystem quota, supervisor blast radius, and operator-run Git maintenance are not source-visible.
- Persistent growth across repeated calls depends on distinct changed file contents; practical workspace sizes and change rates are deployment-dependent.

Safe validation plan:

- **Local:** In the required isolated sandbox with strict CPU, memory, process, disk, output, and wall limits, create a disposable Git worktree with non-secret files at small increasing sizes. Issue one, then two and four concurrent diff calls; record elapsed time, peak RSS, Git child count, and object-store bytes before and after bounded content changes. Stop at the first clear growth point and verify a small shared semaphore plus non-persistent object handling bounds the result.
- **Owner-observed deployment check:** Document effective station cgroup/container ceilings, filesystem quota, supervisor scope, Read-token issuance, and scheduled Git gc/prune policy. Do not stress a production station.

## Rejected hypothesis

Rejected records are retained for audit history and are not findings.

### Same-UID provider access to owner-local sockets is not a distinct authority boundary

- **Fingerprint:** `local-ipc-same-uid-provider-owner-authority`
- **Rejected claim:** Local IPC authenticates only Unix peer UID while a provider child runs under the same UID and can derive the default node path from HOME.
- **Reason:** The proposed attacker is a configured executable running as the same OS principal that the local socket intentionally authenticates, with no source-visible sandbox or lesser filesystem identity. A malicious same-UID binary already has equivalent access to owner-readable configuration and node files, so reaching the socket does not by itself cross a distinct enforced boundary. A model prompt is not shown to gain arbitrary socket I/O without an already-authorized execution capability, and custom data-directory deployments also break the claimed HOME-to-socket step. Process isolation would be a valuable hardening direction, but this candidate lacks a realistic lower-trust entrypoint and incremental unauthorized result.
- **Trace checked:** `crates/fleetdeck-providers/src/host/direct.rs:50` → `crates/fleetdeck-core/src/childenv.rs:67` → `crates/fleetdeck-core/src/localsock.rs:63` → `crates/fleetdeck-station/src/local.rs:439`

## Positive source patterns

- The HTTP method tables default closed and separate owner, member and scoped bearer-token authority; handlers recheck resource membership before sensitive mutations.
- CSRF and Origin checks, session-bound elevation, and one-use or short-lived capabilities reduce browser and link replay risk.
- Deck/station links use pinned mutual TLS identities with negotiated method, scope and direction checks.
- Filesystem reads use allowed-root/private-path policy and no-follow opens in important paths, while artifact rendering is sandboxed without same-origin privilege.
- Coordinator bindings and grants are revalidated, snapshot commits are transactional, and revoked grants are not intentionally revived on the normal path.
- Provider protocol handling rejects unknown methods and invalid answer shapes in key Claude and Codex paths; durable shim output is bounded and generation-fenced.
- Station event acknowledgements are incarnation-bound, monotonic and capped at delivered/committed sequence numbers.
- The Rust workspace forbids unsafe code, and reviewed parsers apply multiple frame, body, queue and cardinality limits.

## Defense-in-depth hardening

These are source hardening directions, not additional findings and not substitutes for resolving the validation queue.

- Add explicit byte and cardinality budgets at every provider, Git, assist, artifact and diff buffering boundary, and terminate only the offending operation when a limit is exceeded.
- Schedule durable retention for audit, event, receipt, recovery, checkpoint and coordinator-outbox state; test pruning against replay and recovery invariants.
- Bind browser drafts, notes and other authoring caches to an authenticated principal or clear them on every logout/account transition.
- Require explicit confirmation before honoring OSC 52 clipboard writes from terminal output.
- Compare complete normalized HTTPS origins, including effective port, before attaching integration credentials.
- Use descriptor-relative, no-follow filesystem mutations for restore, service-file, upgrade-marker and other security-sensitive path operations.
- Persist managed-service executables as validated absolute paths and revalidate current workspace/policy immediately before shell or handover launch.
- Make authentication failure throttles source- or principal-scoped in addition to retaining a conservative global emergency bound.
- Tie cached review policy, queued member work and link snapshots to explicit authorization or lifecycle generations checked at commit time.
- Treat durable revocation and live-upgrade marker failures as fail-closed errors and make the failure visible to the caller.

## Coverage summary

The validated ledger contains **36 deterministic units**: 11 covered, 25 candidate-bearing, 0 blocked, 0 deferred and 0 out of scope. “Candidate-bearing” means the unit produced at least one independently disposed structured record; it does not mean the candidate was confirmed. All 35 candidate fingerprints received an independent Phase 3 disposition, and all 34 retained needs-validation records received a separate Phase 5 source review.

| # | Surface | Boundary | Status | Source checks | Linked records |
| ---: | --- | --- | --- | ---: | --- |
| 1 | Dependencies, CI, packaging, tools and self-upgrade | Contributor/build input to trusted release or installed executable | covered | 1 | — |
| 2 | Service installation, rendering, replacement, removal and supervisor invocation across systemd, launchd and WSL | Owner CLI inputs and ambient paths to persistent service definitions, executable selection, environment inheritance and daemon lifecycle | candidate | 4 | `service.persisted-path-executable-hijack` |
| 3 | Tailnet station join CLI, target resolution, retry file, resume and cancellation | Owner-supplied tailnet target and pinned deck fingerprint to a remote pairing request and durable station trust | covered | 3 | — |
| 4 | CLI, TOML, settings, environment and secrets | Owner configuration to listener, child environment and credential files | candidate | 3 | `config.inline-provider-secret-readable-mode` |
| 5 | Deck/station Unix sockets, coordinator loopback MCP and shim IPC | Local process/model caller to owner, station and provider authority | candidate | 2 | `local-ipc-same-uid-provider-owner-authority` |
| 6 | Connected accounts, OAuth, trackers, redirects and writes | Remote services/project config to stored credentials and outbound actions | candidate | 5 | `integrations.asset.auth-origin-port-confusion` |
| 7 | HTTP admission, bodies, WebSockets and long-lived connections | Network clients to shared deck capacity | candidate | 3 | `http.auth.global-failure-window-lockout` |
| 8 | Mail, task, assignment, plan, grant and schedule HTTP operations | Owner, bearer or member requests to cross-session orchestration, delegated grants and delayed station effects | candidate | 7 | `orchestration.member-mail-stale-share-authority` |
| 9 | Model-help HTTP routes, automatic watches, cache and credentialed provider CLI | Authenticated browser or session-derived content to owner-funded provider execution, provider credentials and persisted generated text | candidate | 7 | `assist.codex-untrusted-prompt-host-read-authority`<br>`assist.runner-unbounded-stdout-capture` |
| 10 | Public authentication and guard-refusal audit writes | Unauthenticated network requests to durable deck SQLite and disk capacity | candidate | 3 | `deck.audit.unauthenticated-refusal-unbounded-retention` |
| 11 | Owner-maintenance HTTP bridges for worktree inventory/destruction and unresolved shim execution inspection/reconciliation | An authenticated Admin-scoped HTTP principal and caller-selected station/session identifiers to owner-only filesystem metadata, destructive worktree mutations, and recovery-state authority on a station | covered | 2 | — |
| 12 | Workspace reads, searches, session/coordinator diffs, pasted images and private-path registry | Member, Drive principal or provider-controlled repository state to private owner file disclosure, browser-visible projections and station temporary storage | candidate | 9 | `workspace.private-path-home-unset-disclosure`<br>`workspace.review-anchor.member-deny-cache-stale`<br>`workspace.session-diff-unthrottled-materialization` |
| 13 | Public and guarded HTTP, WebSocket, shell, artifact and OAuth routes | Network/browser principal to owner, token, member and public resources | covered | 1 | — |
| 14 | Session side chat, presence and mob-driver handoff HTTP/WebSocket lifecycle | Invited member or owner actions to shared-session chat state and current driver authority | candidate | 5 | `collaboration.bearer-token-side-chat-access` |
| 15 | Deck activity, archive, read-cursor, fleet-extra and project/session metadata APIs | Member or scoped bearer requests to per-principal and deck-wide derived metadata, command projections and session/project associations | candidate | 3 | `metadata.read-cursor-token-name-reuse` |
| 16 | Shell commands and data-stream attachment | Owner browser/link stream to station PTY process | candidate | 7 | `shell.handover-native-id-argv-option-confusion`<br>`shell.handover-stale-launch-policy` |
| 17 | Tailscale discovery/automation, listener config, service/container deployment | External helper/config/deployment identity to network exposure and certificate files | covered | 1 | — |
| 18 | Board HTTPS certificate creation, persistence, pinning and device bootstrap links | Filesystem certificate state and owner-issued bootstrap material to browser or CLI authentication of the board endpoint | covered | 3 | — |
| 19 | Workspace discovery and Git preflight HTTP/link routes | Drive-scoped browser or bearer input to owner filesystem metadata and credentialed Git subprocess/network access | candidate | 4 | `workspace.preflight.unbounded-git-output` |
| 20 | Coordinator MCP read tools and station-returned workspace, diff, issue, project and artifact content | Lower-trust repository or external-service content returned to a model binding that can also invoke mutation and fleet-control tools | candidate | 5 | `coordinator.integration-error-untrusted-text-unfenced` |
| 21 | Station event replay, snapshot batches and deck read-model synchronization | Authenticated station event and snapshot streams to deck-wide durable projections, coordinator grants, conflict jobs, timelines and acknowledgments | candidate | 3 | `event-sync.superseded-stale-snapshot-commit` |
| 22 | SQLite commands, projections, outbox, reviews, schedules and recovery | Concurrent/retried/principal-scoped operations to shared durable state | candidate | 3 | `station.upgrade-live-marker-fail-open-rollback` |
| 23 | Pinned-mTLS link, control/data mux, admission and pairing | Network peer to authenticated role, grant and shared daemon capacity | candidate | 5 | `link.revocation-store-failure-peer-revival` |
| 24 | Claude/Codex protocols, direct/shim hosting and resume | Provider/model process to shared station state and capacity | candidate | 7 | `provider-adapter-checkpoint-unbounded-request-state`<br>`provider-stdin-unbounded-response-queue`<br>`provider-stdout-unbounded-line` |
| 25 | Claude and Codex provider protocol frames, control requests, notifications and replies | Provider-controlled JSON and native request identifiers to station events, approval prompts, automatic replies and provider-command correlation | candidate | 3 | `provider.codex-duplicate-native-id-approval-confusion` |
| 26 | Durable provider shim sockets, wire protocol, journal, reattachment and proof | Provider or stale shim process and persisted shim state to the station's live provider session, stdin authority and exactly-once lifecycle | covered | 2 | — |
| 27 | Durable shim pre-authentication connection admission | A same-UID local process, including a lower-trust provider child, to an individual shim process's task and file-descriptor capacity | covered | 3 | — |
| 28 | Artifact file publication, HTML bundle staging and durable version storage | Owner, coordinator or workspace-controlled files to shared station disk, artifact metadata and browser-downloadable bytes | candidate | 6 | `artifacts.path-publication-unbounded-retention-buffering`<br>`artifacts.provider-suggestion-unbounded-durable-cardinality` |
| 29 | Coordinator MCP, grants and fleet-control reads/actions | Model/tool caller to other sessions and approvals | candidate | 5 | `coordinator.outbox-terminal-retention-unbounded` |
| 30 | Station command intake, decision, retry, resume and recovery | Deck/local/coordinator commands to provider and workspace effects | candidate | 5 | `station.drain-prechecked-submit-after-idle`<br>`station.restart-hibernated-input-resume-bypass` |
| 31 | Station event, receipt, effect, process and request persistence | Provider output and accepted member/token commands to shared station database and recovery capacity | candidate | 6 | `station.event-receipt-prune-unscheduled`<br>`station.terminal-recovery-state-unbounded-retention` |
| 32 | Workspace/Git/checkpoint/restore/artifact and integration inputs | Repository/remote/provider-controlled data to owner files, credentials and outbound requests | candidate | 5 | `workspace.restore.path-check-use-race`<br>`integrations.asset.auth-origin-port-confusion` |
| 33 | Workspace provisioning, Git worktree/clone operations, setup commands, checkpoints and trash lifecycle | Session/project commands and repository state to Git subprocesses, setup-shell execution and destructive workspace filesystem changes | covered | 3 | — |
| 34 | Tracked source, config, dependencies and parsers | Source defaults/input handling to secrets, debug surfaces or process integrity | covered | 1 | — |
| 35 | URL fragments, browser storage, WebSocket state, Markdown, artifacts and terminal | Untrusted deck/provider content to victim browser session | candidate | 6 | `browser.local-notes.account-switch-import`<br>`browser.terminal.osc52-unprompted-clipboard-write` |
| 36 | Browser login, reconnect, elevation, account switch and joining | Prior browser state to current authenticated principal | covered | 1 | — |

## Limitations and interpretation

- This is a source audit of one commit, not a penetration test of a deployed fleet. Live listener exposure, firewall and Tailnet ACLs, service-user isolation, filesystem ACLs, OAuth/provider policy, actual provider versions and CI branch protection are deployment facts outside the repository.
- Runtime validation was deliberately not improvised on the host. The missing sandbox controls would have made provider, browser, filesystem-race and resource-exhaustion checks unsafe or non-reproducible.
- The clean coverage critics establish closure of this audit's deterministic ledger, not proof that the codebase contains no undiscovered vulnerability.
- Re-run the relevant units after source, dependency, provider-version, packaging or deployment-policy changes, and convert a lead to a confirmed finding only after reproducing its minimum concrete result with dummy principals/resources under the stated controls.
