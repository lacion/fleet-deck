//! The owner's usage recount (B20, design `usage-cost.md` §4.2): the
//! transcript or rollout total, through the session manager with a real
//! provider home in a temp directory. The refusals are in `refusals`.

#[path = "usage_recount_refusal_tests.rs"]
mod refusals;
#[path = "usage_recount_rig.rs"]
mod rig;

use fleetdeck_proto::event::{ProviderKind, USAGE_SCHEME, UsageSource};
use fleetdeck_proto::{CommandId, RejectCode};
use fleetdeck_providers::codex::thread_usage::TokenTotals;

use rig::*;

#[tokio::test]
async fn a_claude_session_takes_the_transcript_total() {
    let mut fx = Fx::new().await;
    fx.session("s1", ProviderKind::Claude, Some(NATIVE)).await;
    let before = fx.store().summary("s1".into()).await.unwrap().unwrap();
    assert_eq!(before.usage.scheme, None);
    write_private(
        &fx.claude_file(),
        &format!(
            "{}{}{}",
            claude_user(),
            cost_state(10, 0.1),
            cost_state(20, 0.2)
        ),
    );
    let (usage, previous, source, command_id) = set_parts(fx.accepted("s1").await);
    assert_eq!(source, UsageSource::ClaudeTranscript);
    assert_eq!(command_id, CommandId::from("recount-1"));
    assert_eq!(previous, before.usage);
    assert_eq!(usage.scheme, Some(USAGE_SCHEME));
    assert!(usage.tokens_partial && usage.cost_partial);
    assert_eq!(usage.input_tokens, Some(20));
    assert_eq!(usage.output_tokens, Some(2));
    assert_eq!(usage.cache_read_tokens, Some(3));
    assert_eq!(usage.cache_write_tokens, Some(4));
    assert!((usage.cost_usd.unwrap() - 0.2).abs() < 1e-12);
    // The fold replaces the old count and keeps the old one in the recount.
    let after = fx.store().summary("s1".into()).await.unwrap().unwrap();
    assert_eq!(after.usage, usage);
    assert_eq!(after.usage_legacy, None);
    let recount = after.usage_recount.unwrap();
    assert_eq!(recount.source, UsageSource::ClaudeTranscript);
    assert_eq!(recount.command_id, command_id);
    assert_eq!(recount.previous, before.usage);
}

#[tokio::test]
async fn a_codex_session_prices_each_interval_by_its_model() {
    let mut fx = Fx::new().await;
    fx.session("s1", ProviderKind::Codex, Some(NATIVE)).await;
    write_private(
        &fx.codex_file(),
        &format!(
            "{}{}{}{}{}",
            codex_meta(),
            codex_context("gpt-5"),
            codex_count(1000, 400, 50),
            codex_context("gpt-5-mini"),
            codex_count(3000, 1400, 150),
        ),
    );
    let (usage, previous, source, _) = set_parts(fx.accepted("s1").await);
    assert_eq!(source, UsageSource::CodexRollout);
    assert_eq!(previous, old_count());
    let cost = |model: &str, input, cached, output| {
        TokenTotals {
            input,
            cached,
            cache_write: 0,
            output,
        }
        .usage(Some(model))
        .cost_usd
        .unwrap()
    };
    let want = cost("gpt-5", 1000, 400, 50) + cost("gpt-5-mini", 2000, 1000, 100);
    assert!((usage.cost_usd.unwrap() - want).abs() < 1e-12, "{usage:?}");
    assert_eq!(usage.input_tokens, Some(3000 - 1400));
    assert_eq!(usage.cache_read_tokens, Some(1400));
    assert_eq!(usage.output_tokens, Some(150));
    assert_eq!(usage.scheme, Some(USAGE_SCHEME));
    assert!(usage.tokens_partial && usage.cost_partial);
}

#[tokio::test]
async fn no_record_sets_an_unknown_total() {
    let mut fx = Fx::new().await;
    fx.session("s1", ProviderKind::Claude, Some(NATIVE)).await;
    // No transcript file.
    let (usage, previous, source, _) = set_parts(fx.accepted("s1").await);
    assert_eq!(source, UsageSource::None);
    assert_eq!(previous, old_count());
    assert_eq!(usage.scheme, Some(USAGE_SCHEME));
    assert_eq!(usage.input_tokens, None);
    assert_eq!(usage.output_tokens, None);
    assert_eq!(usage.cost_usd, None);
    assert!(usage.tokens_partial && usage.cost_partial);
}

#[tokio::test]
async fn a_session_without_a_native_id_has_no_record() {
    let mut fx = Fx::new().await;
    fx.session("s1", ProviderKind::Claude, None).await;
    let (usage, _, source, _) = set_parts(fx.accepted("s1").await);
    assert_eq!(source, UsageSource::None);
    assert_eq!(usage.cost_usd, None);
}

#[tokio::test]
async fn a_transcript_without_a_cost_state_record_has_no_record() {
    let mut fx = Fx::new().await;
    fx.session("s1", ProviderKind::Claude, Some(NATIVE)).await;
    write_private(&fx.claude_file(), &claude_user());
    let (_, _, source, _) = set_parts(fx.accepted("s1").await);
    assert_eq!(source, UsageSource::None);
}

#[tokio::test]
async fn an_untrusted_file_is_refused() {
    let mut fx = Fx::new().await;
    fx.session("s1", ProviderKind::Claude, Some(NATIVE)).await;
    let real = fx.root.join("real.jsonl");
    write_private(&real, &format!("{}{}", claude_user(), cost_state(20, 0.2)));
    let link = fx.claude_file();
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let message = fx
        .refused("s1", RejectCode::InvalidState, "record_unreadable")
        .await;
    assert!(message.contains("not_trusted"), "{message}");
}

/// Sets the mode of `path`.
fn chmod(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// The owner can read every file as root: a test of no access then proves
/// nothing.
fn is_root() -> bool {
    rustix::process::geteuid().is_root()
}

/// The refusal keeps the old count and no `usage.set` is written.
async fn assert_old_count_kept(fx: &Fx) {
    let summary = fx.store().summary("s1".into()).await.unwrap().unwrap();
    assert_eq!(summary.usage, old_count());
    assert_eq!(summary.usage.scheme, None);
    assert!(summary.usage_recount.is_none());
}

#[tokio::test]
async fn a_provider_home_without_access_is_refused_and_a_retry_counts() {
    if is_root() {
        return;
    }
    let mut fx = Fx::new().await;
    fx.session("s1", ProviderKind::Claude, Some(NATIVE)).await;
    write_private(
        &fx.claude_file(),
        &format!("{}{}", claude_user(), cost_state(20, 0.2)),
    );
    // No search right on the directory that holds the home: the home
    // exists, but it cannot be resolved.
    chmod(&fx.root, 0o600);
    let message = fx
        .refused("s1", RejectCode::InvalidState, "record_unreadable")
        .await;
    // The access returns: a retry reads the record.
    chmod(&fx.root, 0o700);
    assert!(message.contains("not_trusted"), "{message}");
    assert_old_count_kept(&fx).await;
    let (usage, previous, source, _) = set_parts(fx.accepted("s1").await);
    assert_eq!(source, UsageSource::ClaudeTranscript);
    assert_eq!(previous, old_count());
    assert_eq!(usage.input_tokens, Some(20));
}

#[tokio::test]
async fn a_codex_directory_without_access_is_refused_and_a_retry_counts() {
    if is_root() {
        return;
    }
    let mut fx = Fx::new().await;
    fx.session("s1", ProviderKind::Codex, Some(NATIVE)).await;
    write_private(
        &fx.codex_file(),
        &format!(
            "{}{}{}",
            codex_meta(),
            codex_context("gpt-5"),
            codex_count(1000, 400, 50)
        ),
    );
    // The month directory of the rollout cannot be opened.
    let month = fx.root.join(".codex/sessions/2026/10");
    chmod(&month, 0o000);
    let message = fx
        .refused("s1", RejectCode::InvalidState, "record_unreadable")
        .await;
    chmod(&month, 0o700);
    assert!(message.contains("not_trusted"), "{message}");
    assert_old_count_kept(&fx).await;
    let (usage, _, source, _) = set_parts(fx.accepted("s1").await);
    assert_eq!(source, UsageSource::CodexRollout);
    assert_eq!(usage.output_tokens, Some(50));
}
