//! The clap types of `fleetdeck omarchy` (R13 P4 task C, `omarchy-p4.md`
//! §3.4).

use clap::Subcommand;

/// `fleetdeck omarchy <command>`.
#[derive(Debug, Subcommand)]
pub enum OmarchyCommand {
    /// Install the Fleet Deck plugin, hooks, theme template and hotkeys into
    /// the Omarchy configuration of this user.
    Install {
        /// Do not ask; needed when there is no terminal.
        #[arg(long)]
        yes: bool,
        /// Also record that the ambient screensaver is on.
        #[arg(long)]
        screensaver: bool,
    },
    /// Remove what install wrote (a file that you changed stays).
    Uninstall,
    /// Show the files, the plugin, the fleet summary and the Hyprland line.
    /// The exit code is 0 only when nothing changed or is missing.
    Status {
        /// Print one JSON line.
        #[arg(long)]
        json: bool,
    },
}
