# Fleet Deck v2 — decisions log

This log records each deviation from PLAN.md and each choice that the plan does not state. The goal loop runs without owner access, so each entry has "owner review: pending" until the owner accepts or changes it.

Format: context, options, choice, reason, owner review.

---

## DL-001 — Cargo workspace in Phase 0 holds only the spike crates

- **Context:** PLAN §12 places the workspace `Cargo.toml` at the repository root with the product crates under `crates/`. Phase 1 creates those crates. The M1 checks (`cargo fmt --all --check`, `cargo clippy --workspace`, `cargo nextest run --workspace`, `cargo deny check`) must run on the spike crates in Phase 0.
- **Options:** (a) a separate workspace in `spikes/Cargo.toml`; (b) the root workspace now, with members `spikes/*` only.
- **Choice:** (b). The root `Cargo.toml` lists only `spikes/*` members in M1. Phase 1 adds `crates/*` and decides if the spikes stay members or move to `exclude`.
- **Reason:** the checks in the goal run from the repository root. One workspace gives one `Cargo.lock`, one `deny.toml`, and one lint policy that Phase 1 reuses.
- **Owner review:** pending

## DL-002 — MSRV pin without a `rust-toolchain.toml`

- **Context:** PLAN §12 asks for "stable, edition 2024, pinned MSRV". The installed stable toolchain is 1.96.0. A `rust-toolchain.toml` with `1.96` makes rustup install a second named toolchain (network access, a slow first build).
- **Options:** (a) `rust-toolchain.toml` with `channel = "1.96"`; (b) `rust-version = "1.96"` in `[workspace.package]` only.
- **Choice:** (b) for M1. Phase 1 CI can add the toolchain file when CI installs toolchains anyway.
- **Reason:** `rust-version` pins the MSRV for cargo (it rejects an older compiler), and the local checks run on the installed stable 1.96.0 without a download.
- **Owner review:** pending

## DL-003 — Claude permission mode names differ from PLAN §6.2

- **Context:** PLAN §6.2 lists the Claude modes `default` / `acceptEdits` / `plan` / `bypassPermissions`. `claude --help` of 2.1.289 lists `acceptEdits`, `auto`, `bypassPermissions`, `manual`, `dontAsk`, `plan`. There is no `default`.
- **Options:** (a) keep `default` and translate it; (b) use the native names of the installed CLI.
- **Choice:** (b). The S1 adapter offers exactly the native names (L8) and uses `manual` as the most restrictive mode for an unknown stored mode. The capability matrix records the verified names. No PLAN decision changes: only the matrix value column changes.
- **Reason:** L8 ("offer only the modes that the provider enforces natively"). A translated sentinel name is trap 1/2 of t3code-lessons §5.
- **Owner review:** pending

## DL-004 — The driven Claude session always passes `--permission-mode`

- **Context:** on this machine the user's Claude settings make `bypassPermissions` the default mode (a first live probe showed `"permissionMode":"bypassPermissions"` in `system/init`). A driven session that passes no mode inherits it.
- **Options:** (a) inherit the user's default; (b) always pass an explicit native mode, `manual` when none is stored.
- **Choice:** (b). `cli_args()` always adds `--permission-mode`.
- **Reason:** the goal forbids bypass mode outside a temporary directory, and L8 requires the most restrictive mode for an unknown stored mode. A station must not run with a mode that the owner did not choose.
- **Owner review:** pending
