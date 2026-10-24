//! The version probe command of the Codex driver.

use fleetdeck_proto::event::ProviderKind;

use super::CodexAdapter;
use crate::apps::apps_verified;
use crate::codex::parse_version;

impl CodexAdapter {
    /// Runs one probe command. `Ok(None)`: it did not start or timed out.
    pub(super) async fn run_probe(&self, args: &[&str]) -> Option<(bool, String)> {
        let mut cmd = tokio::process::Command::new(&self.config.program);
        cmd.args(args)
            .env_clear()
            .envs(self.config.env.iter())
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let run = tokio::time::timeout(self.config.probe_timeout, cmd.output())
            .await
            .ok()?;
        run.ok().map(|o| {
            (
                o.status.success(),
                String::from_utf8_lossy(&o.stdout).into_owned(),
            )
        })
    }

    /// Whether this Codex gets the MCP Apps extension (B15): `codex --version`
    /// once, at or above [`crate::apps::APPS_VERIFIED`]. Only a parsed version
    /// is cached; a probe that fails or times out answers `false` and is tried
    /// again at the next start.
    pub(super) async fn apps_offered(&self) -> bool {
        let probed = self
            .offer_apps
            .get_or_try_init(|| async {
                let (ok, out) = self.run_probe(&["--version"]).await.ok_or(())?;
                let version = ok.then(|| parse_version(&out)).flatten().ok_or(())?;
                Ok::<bool, ()>(apps_verified(ProviderKind::Codex, Some(&version)))
            })
            .await;
        probed.copied().unwrap_or(false)
    }
}
