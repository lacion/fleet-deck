//! The version probe command of the Codex driver.

use super::CodexAdapter;

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
}
