# Shim host parity with the direct host (DL-188)

- **Status:** Plan (2026-10-15). Not designed in detail, not built. Each step below gets its own design (or brief) and the reviews named in §4 before code.
- **Owner request (2026-10-15):** "could we make shim support all of that? could we have parity with direct and on top add all the other stuff we said?" and "save that to a plan in docs so we can work on that later".
- **Goal:** a session on the shim host (`host = "shim"`) can do everything that a session on the direct host can do. On top of that, its agent keeps running through a station restart, an upgrade or a station crash (ADR-005). When parity is reached, the owner decides whether `shim` becomes the default host.

## 1. Why the shim host exists

The direct host runs each agent as a child of the station. A station stop kills every agent at once: a running tool call, a long test run or a half-done edit stops. Auto-continue starts the conversation again from its last saved point, but the cut turn is lost, and commands in flight end as `outcome_unknown`.

The shim host runs each agent under a `fleetdeck shim` process that keeps the agent and its output. A new station process reattaches (ADR-005). This matters for long, unattended agent runs on a station that restarts often (new builds through `fleetdeck service install`, settings that need a restart, upgrades).

## 2. Gaps today (code facts, 2026-10-15, `v2` at `80463494`)

| # | Gap on the shim host | Where | Cause |
| --- | --- | --- | --- |
| G1 | Coordinator sessions are refused ("a resume or a shim-hosted start is refused"; operations guide §11). | `manager/coordinator_start.rs`, `manager/target.rs` | The coordinator grant credential reaches the agent at spawn. On the shim host the shim spawns the agent, and ADR-005 keeps credentials out of the durable launch journal and the station database. |
| G2 | Session tools (`publish_artifact`, `set_focus`, `ask_coordinator`; DL-165) are missing on every shim session. | `manager/coordinator_start.rs` `NoTools::Durable` | The same as G1: no handoff path for the session credential. |
| G3 | Turn revert is refused for Claude sessions (`revert_not_supported_on_the_shim_host`). Codex works (`CodexRollback`). | `manager/shim_target.rs` `REASON_REVERT_UNSUPPORTED` | A Claude revert restarts the agent at a checkpoint; that path was never built for the shim host. |
| G4 | Auto-continue is off on shim stations; a lost shim session waits for the owner. | `manager/auto_continue.rs` (`shim_host` → ineligible) | A lost shim session may resume only with proof that its old run ended (DL-182). Auto-continue does not use that proof yet. |
| G5 | macOS: no shim session can start, so also no import and no resume ("resume on the shim host needs Linux"). | `fleetdeck-providers/src/host/shim/identity.rs` (`Unsupported`), `manager/policy.rs` `SHIM_HOST_NEEDS_LINUX` | The process identity (`pid`, start time, boot id) and the process-group census read `/proc`. The macOS calls are not in the build. |
| G6 | The settings accept `host = "shim"` on macOS, although no shim session can start there. The import refusal does not say what to do. | `fleetdeck-core/src/settings/fields.rs` (`host`), `manager/policy.rs` | Nothing checks the platform when the host is chosen. |

Not gaps (kept on purpose): a resume on the shim host needs proof that the earlier run ended (DL-182; it prevents two agents on one conversation); descendants that leave the agent's process group are not tracked (the same as on the direct host). Not about the shim host: a Codex coordinator start fails with `handoff_unverified` on both hosts.

## 3. Steps

Each step is merged on `v2` on its own, with `make check` green, and reported to the owner.

0. **Guard (small, G6).** A station on macOS refuses `host = "shim"` at the settings save (station and board) with: "The shim host needs Linux. Use `direct` on this station." A station that starts on macOS with `host = "shim"` in `station.toml` logs one warning and runs the direct host. The import and resume refusal says what to do: "Import and resume need Linux on a shim host. On this station, set Host to `direct` in its settings."
   - **Done (2026-10-15):** the guard (be-shim0).
1. **Credential handoff (G1, G2).** The station sends the session credential to the shim in the launch message over the shim's owner-only socket. The shim gives it to the agent's environment and never writes it to the launch journal, a log or the database. A reattach needs no credential (the agent already has it). A resume or a relaunch mints a new credential. Then coordinator sessions and session tools work on the shim host. Security surface: a written design, an adversary DESIGN REVIEW before code, a PRE-PR review, and a row in `SECURITY-CHECKLIST.md`.
   - **Done (2026-10-16):** DL-194 (`docs/v2/design/shim-credential.md` revision 6), Claude on Linux; Codex waits for a live smoke of `env_vars` (§9.10). PRE-PR review: round 2 PASS.
2. **Claude turn revert (G3).** Build the Claude revert on the shim host with the durable resume launch of DL-182 (`ProviderAdapter::durable_resume_launch`) at the checkpoint, with the same file restore and proofs as the direct host.
3. **Auto-continue (G4).** A lost shim session is auto-continued when the station holds the DL-182 proof that its run ended (a clean exit that the shim checked, a launch that never started, a verified reboot). Without proof, the session waits for the owner, as today. The same rechecks as the direct host apply.
4. **macOS (G5).** Port the identity and the census: start time through `proc_pidinfo(PROC_PIDTBSDINFO)`, the boot identity through the `kern.bootsessionuuid` sysctl, and the process-group and session scan through `proc_listpids`. Owner decisions before this step (§5). The owner runs the macOS acceptance script on a Mac (this build machine cannot run macOS).
5. **Default host (owner decision).** When steps 1 to 4 are done, the owner decides whether `shim` becomes the default and `direct` the fallback.

## 4. Reviews

| Step | Risk | Review |
| --- | --- | --- |
| 0 | Low | Diff check and tests. |
| 1 | High (credentials) | Adversary DESIGN REVIEW, then PRE-PR REVIEW. |
| 2 | Normal (file restore) | One Codex review of the branch. |
| 3 | Normal | One Codex review of the branch. |
| 4 | High (process identity is the proof against two agents on one conversation) | Adversary DESIGN REVIEW, then PRE-PR REVIEW, plus the owner's Mac run. |

## 5. Open owner decisions

- **macOS calls (step 4):** the workspace forbids `unsafe` code. Option A: a small, well-known crate for the process calls (for example `libproc`); correct, but a new dependency (`cargo deny` and the owner's approval). Option B: run `ps` and `sysctl` and parse their output; no new dependency, but fragile. Architect recommendation: A.
- **Decision (owner, 2026-10-15):** option A, the `libproc` crate.
- **Default host (step 5):** after parity.
