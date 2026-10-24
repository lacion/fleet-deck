//! Entry points of the fake agents (PLAN §13: scripted fakes cover only the
//! cases that a recording cannot produce, for example crashes and malformed
//! frames).
//!
//! The fakes are binaries of this crate (`fake-claude`, `fake-codex`), so the
//! station starts them through the same `SessionHost` code path as a real
//! CLI. A test selects a scenario with [`SCENARIO_ENV`].
//!
//! Finding the binaries: inside this crate's integration tests Cargo sets
//! `CARGO_BIN_EXE_<name>`. Tests of other crates call [`FakeAgent::locate`],
//! which uses [`BIN_DIR_ENV`] first and then the Cargo target directory of
//! the running test (`cargo nextest run --workspace` and
//! `cargo test --workspace` build the fakes).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Environment variable that selects the scenario of a fake agent.
pub const SCENARIO_ENV: &str = "FLEETDECK_FAKE_SCENARIO";
/// Environment variable with a number of milliseconds: the Claude `sim`
/// scenario waits that long after stdin EOF before it exits, like a CLI
/// that an async user hook keeps alive (DL-141). A signal still ends it at
/// once.
pub const LINGER_ENV: &str = "FLEETDECK_FAKE_LINGER_MS";
/// Environment variable with a file path: the `sim` scenario appends every
/// stdin line to it, so fault tests can count what reached the provider
/// (after one `PID_LINE` with its process id).
pub const STDIN_LOG_ENV: &str = "FLEETDECK_FAKE_STDIN_LOG";
/// Environment variable with a file path: a fake agent writes its
/// arguments there, one per line, then `env:` and the value of
/// `FLEETDECK_COORDINATOR_TOKEN` (coordinator handoff tests, DL-216).
pub const LAUNCH_LOG_ENV: &str = "FLEETDECK_FAKE_LAUNCH_LOG";
/// Environment variable with a fault of a resumed fake (FG-9, T4.2d). It
/// applies only to a process that resumes a conversation: `fake-claude sim`
/// started with `--resume`, or `fake-codex` answering `thread/resume`; any
/// other start ignores it. Values: [`RESUME_EXIT_AT_START`],
/// [`RESUME_MISSING`], [`RESUME_OTHER_ID`].
pub const RESUME_FAULT_ENV: &str = "FLEETDECK_FAKE_RESUME_FAULT";
/// The Claude `sim` exits with code 1 at once, before it reads stdin, after a
/// line on stderr.
pub const RESUME_EXIT_AT_START: &str = "exit_at_start";
/// The Claude `sim` prints `No conversation found with session ID: <id>` on
/// stderr and exits with code 1 at the first user message.
pub const RESUME_MISSING: &str = "missing";
/// The Claude `sim` uses `sim-other-<pid>` as its session id; the Codex `sim`
/// answers `thread/resume` with the thread id `other-<pid>`.
pub const RESUME_OTHER_ID: &str = "other_id";
/// Environment variable that overrides the directory of the fake binaries.
pub const BIN_DIR_ENV: &str = "FLEETDECK_FAKE_BIN_DIR";

/// The fake agent of one provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FakeAgent {
    Claude,
    Codex,
}

#[derive(Debug, thiserror::Error)]
pub enum FakeError {
    #[error(
        "fake agent `{name}` not found in {dir}; build it with \
         `cargo build -p fleetdeck-testkit --bins` or set {BIN_DIR_ENV}"
    )]
    NotFound { name: &'static str, dir: PathBuf },
    #[error("cannot find the test executable: {0}")]
    CurrentExe(#[from] std::io::Error),
}

impl FakeAgent {
    pub const ALL: [Self; 2] = [Self::Claude, Self::Codex];

    /// The binary name.
    #[must_use]
    pub fn bin_name(self) -> &'static str {
        match self {
            Self::Claude => "fake-claude",
            Self::Codex => "fake-codex",
        }
    }

    /// The `driver` value in `station.toml`.
    #[must_use]
    pub fn driver(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// The provider instance id that [`crate::TempNode::station_toml`] uses.
    #[must_use]
    pub fn instance_id(self) -> &'static str {
        self.bin_name()
    }

    /// The `--version` line, in the format of the real CLI.
    #[must_use]
    pub fn version_line(self) -> &'static str {
        match self {
            Self::Claude => "2.1.999 (Claude Code)",
            Self::Codex => "codex-cli 0.160.999",
        }
    }

    /// The directory with the fake binaries.
    pub fn bin_dir() -> Result<PathBuf, FakeError> {
        if let Some(dir) = std::env::var_os(BIN_DIR_ENV).filter(|d| !d.is_empty()) {
            return Ok(PathBuf::from(dir));
        }
        let exe = std::env::current_exe()?;
        Ok(target_dir_of(&exe))
    }

    /// The path of the fake binary. Without [`BIN_DIR_ENV`], the first call
    /// in a test process builds the fakes (a no-op when they are fresh), so
    /// `cargo test -p <crate>` never runs a missing or stale fake.
    pub fn locate(self) -> Result<PathBuf, FakeError> {
        let dir = Self::bin_dir()?;
        if std::env::var_os(BIN_DIR_ENV).is_none_or(|d| d.is_empty()) {
            build_once(&dir);
        }
        let path = dir.join(self.bin_name());
        // Another test process's `cargo build` can be replacing the binary
        // at this moment (cargo removes, then links the new file): wait
        // briefly before reporting it missing.
        for _ in 0..100 {
            if path.is_file() {
                return Ok(path);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Err(FakeError::NotFound {
            name: self.bin_name(),
            dir,
        })
    }

    /// A command that starts the fake with `scenario`.
    pub fn command(self, scenario: &str) -> Result<Command, FakeError> {
        let mut cmd = Command::new(self.locate()?);
        cmd.env(SCENARIO_ENV, scenario);
        Ok(cmd)
    }
}

/// The `main` of a fake binary.
///
/// - `--version` prints [`FakeAgent::version_line`].
/// - Scenario `exit:<code>` exits at once with `<code>`.
/// - Scenario `hang` reads stdin until it closes, then exits 0 (for stop and
///   kill tests).
/// - Scenario `echo` copies stdin lines to stdout until stdin closes.
/// - Scenario `deaf` never reads stdin and never exits (until a signal).
/// - Arguments `auth status` print the sign-in state of scenario
///   `auth:signed-in` / `auth:signed-out`, else fail.
/// - Scenario `env` prints its environment (`NAME=value`, sorted) and exits 0
///   (for the environment scrub tests).
/// - `fake-codex app-server` under scenario `sim`, `mcp_app` or `auth:<state>`
///   runs [`crate::sim_codex`] (`mcp_app` is B15: the demo MCP app, and the
///   version `0.163.0`).
/// - Scenario `fixture:<path>#<process>` plays one process of a recorded
///   fixture ([`crate::script`]).
/// - Every other scenario is an error (exit 2).
#[must_use]
pub fn fake_main(agent: FakeAgent) -> std::process::ExitCode {
    use std::io::Read;
    use std::process::ExitCode;

    let name = agent.bin_name();
    if std::env::args().skip(1).any(|a| a == "--version") {
        // Scenario `mcp_app` (B15) reports the first version with an app path.
        let apps =
            agent == FakeAgent::Codex && std::env::var(SCENARIO_ENV).as_deref() == Ok("mcp_app");
        println!(
            "{}",
            if apps {
                "codex-cli 0.163.0"
            } else {
                agent.version_line()
            }
        );
        return ExitCode::SUCCESS;
    }
    let scenario = std::env::var(SCENARIO_ENV).unwrap_or_default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(path) = std::env::var_os(LAUNCH_LOG_ENV) {
        let token = std::env::var("FLEETDECK_COORDINATOR_TOKEN").unwrap_or_default();
        let log = format!("{}\nenv:{token}\n", args.join("\n"));
        if std::fs::write(&path, log).is_err() {
            eprintln!("{name}: cannot write the launch log");
            return ExitCode::from(2);
        }
    }
    if args.iter().map(String::as_str).eq(["auth", "status"]) {
        return auth_status(&scenario);
    }
    if let Some(code) = scenario.strip_prefix("exit:") {
        return match code.parse::<u8>() {
            Ok(code) => ExitCode::from(code),
            Err(_) => {
                eprintln!("{name}: bad exit code in scenario `{scenario}`");
                ExitCode::from(2)
            }
        };
    }
    if let Some(spec) = scenario.strip_prefix(crate::script::FIXTURE_PREFIX) {
        return crate::script::play(name, spec);
    }
    match scenario.as_str() {
        "hang" => {
            let mut sink = Vec::new();
            return match std::io::stdin().read_to_end(&mut sink) {
                Ok(_) => ExitCode::SUCCESS,
                Err(_) => ExitCode::from(1),
            };
        }
        "echo" => return echo_lines(),
        "sim" if agent == FakeAgent::Claude => return crate::sim_claude::run(),
        "sim" | "mcp_app" | "auth:signed-in" | "auth:signed-out"
            if agent == FakeAgent::Codex
                && args.first().map(String::as_str) == Some("app-server") =>
        {
            return crate::sim_codex::run(&scenario);
        }
        "deaf" => loop {
            std::thread::park();
        },
        "env" => {
            let mut vars: Vec<String> = std::env::vars_os()
                .map(|(k, v)| format!("{}={}", k.to_string_lossy(), v.to_string_lossy()))
                .collect();
            vars.sort();
            println!("{}", vars.join("\n"));
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    eprintln!("{name}: unknown scenario `{scenario}` (set {SCENARIO_ENV})");
    ExitCode::from(2)
}

/// `auth status` (Claude JSON form): scenario `auth:signed-in` or
/// `auth:signed-out`; every other scenario fails (exit 1, no output).
fn auth_status(scenario: &str) -> std::process::ExitCode {
    match scenario {
        "auth:signed-in" => println!(r#"{{"loggedIn":true,"authMethod":"claude.ai"}}"#),
        "auth:signed-out" => println!(r#"{{"loggedIn":false}}"#),
        _ => return std::process::ExitCode::from(1),
    }
    std::process::ExitCode::SUCCESS
}

/// Copies stdin lines to stdout until the end of input, then exits 0.
fn echo_lines() -> std::process::ExitCode {
    use std::io::{BufRead, Write};
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else {
            return std::process::ExitCode::from(1);
        };
        if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
            return std::process::ExitCode::from(1);
        }
    }
    std::process::ExitCode::SUCCESS
}

/// Builds the fake binaries into `bin_dir` (`<target>/debug`) once per
/// process. A failed build is not an error here: [`FakeAgent::locate`] then
/// reports the missing binary.
fn build_once(bin_dir: &Path) {
    static BUILT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    BUILT.get_or_init(|| {
        let (Some(target), Some("debug")) = (
            bin_dir.parent(),
            bin_dir.file_name().and_then(|n| n.to_str()),
        ) else {
            return;
        };
        let cargo = std::env::var_os("CARGO")
            .or_else(|| option_env!("CARGO").map(Into::into))
            .unwrap_or_else(|| "cargo".into());
        let status = Command::new(cargo)
            .args([
                "build",
                "--quiet",
                "--offline",
                "-p",
                "fleetdeck-testkit",
                "--bins",
            ])
            .arg("--target-dir")
            .arg(target)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .stdin(std::process::Stdio::null())
            .status();
        if !status.as_ref().is_ok_and(|s| s.success()) {
            eprintln!("fleetdeck-testkit: building the fake agents failed: {status:?}");
        }
    });
}

/// `target/<profile>` for a test executable in `target/<profile>/deps`, or
/// the executable's own directory otherwise.
fn target_dir_of(exe: &Path) -> PathBuf {
    let dir = exe.parent().unwrap_or(Path::new("."));
    if dir.file_name().is_some_and(|n| n == "deps") {
        dir.parent().unwrap_or(dir).to_path_buf()
    } else {
        dir.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_executables_in_deps_map_to_the_profile_directory() {
        assert_eq!(
            target_dir_of(Path::new("/w/target/debug/deps/fleetdeck_station-abc")),
            PathBuf::from("/w/target/debug")
        );
        assert_eq!(
            target_dir_of(Path::new("/w/target/debug/fake-claude")),
            PathBuf::from("/w/target/debug")
        );
    }

    #[test]
    fn names_match_the_station_config_drivers() {
        for agent in FakeAgent::ALL {
            assert!(agent.bin_name().ends_with(agent.driver()));
            assert_eq!(agent.instance_id(), agent.bin_name());
        }
    }

    #[test]
    fn a_missing_binary_names_the_build_command() {
        let err = FakeError::NotFound {
            name: "fake-codex",
            dir: PathBuf::from("/nowhere"),
        };
        let text = err.to_string();
        assert!(
            text.contains("cargo build -p fleetdeck-testkit --bins"),
            "{text}"
        );
        assert!(text.contains(BIN_DIR_ENV), "{text}");
    }
}
