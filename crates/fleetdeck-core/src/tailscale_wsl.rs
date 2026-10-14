//! Tailscale on WSL (tailnet design section 14.1): the host facts that the
//! deck reads, and the rule for the hint about the Windows program.
//!
//! The deck never runs `tailscale.exe` of Windows unless the setting
//! `tailscale_bin` names it. A metadata check of the file is all that
//! happens without that setting.

use std::path::{Path, PathBuf};

use crate::local_addrs;

/// The Tailscale program of Windows, as WSL sees it.
pub const WINDOWS_PROGRAM: &str = "/mnt/c/Program Files/Tailscale/tailscale.exe";

const OSRELEASE: &str = "/proc/sys/kernel/osrelease";
const INTEROP: &str = "/proc/sys/fs/binfmt_misc/WSLInterop";

/// The facts about this machine that the tailnet routes and the automatic
/// start use. The tests give their own facts.
#[derive(Debug, Clone, Default)]
pub struct HostFacts {
    /// The process runs in WSL.
    pub wsl: bool,
    /// `tailscale.exe` of Windows, when this is WSL and the file exists.
    pub windows_program: Option<PathBuf>,
    /// `ip_nonlocal_bind` is on: a bind does not prove a local address.
    pub nonlocal_bind: bool,
}

/// True when the kernel text names WSL (`microsoft` or `wsl`, in any case)
/// and WSL interop is registered. One of the two alone is not WSL.
#[must_use]
pub fn wsl_from(osrelease: &str, interop: bool) -> bool {
    let kernel = osrelease.to_ascii_lowercase();
    interop && (kernel.contains("microsoft") || kernel.contains("wsl"))
}

/// True when this process runs in WSL: the kernel says so and the interop
/// file exists. An environment variable alone never counts.
#[must_use]
pub fn is_wsl() -> bool {
    let kernel = std::fs::read_to_string(OSRELEASE).unwrap_or_default();
    wsl_from(&kernel, Path::new(INTEROP).exists())
}

/// `tailscale.exe` of Windows when this is WSL and the path is a regular
/// file (`symlink_metadata`; nothing is run).
#[must_use]
pub fn windows_hint() -> Option<PathBuf> {
    is_wsl().then(windows_file).flatten()
}

fn windows_file() -> Option<PathBuf> {
    std::fs::symlink_metadata(WINDOWS_PROGRAM)
        .is_ok_and(|m| m.file_type().is_file())
        .then(|| PathBuf::from(WINDOWS_PROGRAM))
}

/// The real facts of this machine.
#[must_use]
pub fn detect() -> HostFacts {
    let wsl = is_wsl();
    HostFacts {
        wsl,
        windows_program: if wsl { windows_file() } else { None },
        nonlocal_bind: local_addrs::nonlocal_bind(),
    }
}

/// The Windows program to offer on the Tailnet card: `Some` only when no
/// `tailscale_bin` is set, no program is found (`PATH`, the macOS app) and
/// the facts have the Windows program. A set `tailscale_bin` never gets it.
#[must_use]
pub fn windows_choice(
    configured: Option<&Path>,
    found_on_path: bool,
    facts: &HostFacts,
) -> Option<PathBuf> {
    if configured.is_some() || found_on_path {
        return None;
    }
    facts.windows_program.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl_needs_the_kernel_text_and_the_interop_file() {
        assert!(wsl_from("5.15.167.4-microsoft-standard-WSL2", true));
        assert!(wsl_from("6.6.87.2-Microsoft-standard-WSL2", true));
        assert!(wsl_from("4.4.0-19041-Microsoft", true));
        assert!(wsl_from("5.10.0-wsl", true));
        // The kernel text alone, or the interop file alone, is not WSL.
        assert!(!wsl_from("5.15.167.4-microsoft-standard-WSL2", false));
        assert!(!wsl_from("6.8.0-45-generic", true));
        assert!(!wsl_from("", true));
        assert!(!wsl_from("", false));
    }

    #[test]
    fn the_windows_program_is_offered_only_when_nothing_else_is_set_or_found() {
        let facts = HostFacts {
            wsl: true,
            windows_program: Some(PathBuf::from(WINDOWS_PROGRAM)),
            nonlocal_bind: false,
        };
        assert_eq!(
            windows_choice(None, false, &facts),
            Some(PathBuf::from(WINDOWS_PROGRAM))
        );
        // A program on PATH wins.
        assert_eq!(windows_choice(None, true, &facts), None);
        // A set `tailscale_bin` wins, also when the file is missing.
        assert_eq!(
            windows_choice(Some(Path::new("/opt/missing/tailscale")), false, &facts),
            None
        );
        // No Windows program in the facts: nothing to offer.
        assert_eq!(windows_choice(None, false, &HostFacts::default()), None);
    }

    #[test]
    fn the_default_facts_are_a_plain_machine() {
        let facts = HostFacts::default();
        assert!(!facts.wsl && !facts.nonlocal_bind);
        assert!(facts.windows_program.is_none());
    }
}
