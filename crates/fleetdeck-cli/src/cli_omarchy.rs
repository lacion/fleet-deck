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
        #[arg(
            long,
            conflicts_with_all = ["replace_conflicts", "plan_stdin", "resume"]
        )]
        screensaver: bool,
        /// Replace the files of a reviewed plan (read from stdin) and keep a
        /// copy of each. Needs --plan-stdin; prints JSON on stdout.
        #[arg(long, requires = "plan_stdin")]
        replace_conflicts: bool,
        /// Read the reviewed plan (one JSON object, at most 64 KiB) from stdin.
        #[arg(long, requires = "replace_conflicts")]
        plan_stdin: bool,
        /// Finish an incomplete repair; prints JSON on stdout.
        #[arg(
            long,
            value_name = "REPAIR_ID",
            conflicts_with_all = ["replace_conflicts", "plan_stdin"]
        )]
        resume: Option<String>,
    },
    /// Print the facts of the install plan as JSON: the files that differ
    /// from the assets (read only).
    InstallPlan {
        /// Accepted for clarity; the output is always JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print the last repairs, from the files on disk, as JSON.
    RepairStatus {
        /// Accepted for clarity; the output is always JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print the restore plan of a repair as JSON (read only).
    RestorePlan {
        /// The repair ID (32 lowercase hexadecimal characters).
        #[arg(long)]
        repair: String,
        /// Accepted for clarity; the output is always JSON.
        #[arg(long)]
        json: bool,
    },
    /// Restore the kept copies of a reviewed restore plan (read from stdin).
    Restore {
        /// Read the restore plan (one JSON object, at most 64 KiB) from stdin.
        #[arg(long, required = true)]
        plan_stdin: bool,
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
    /// Send the Omarchy theme palette of this machine to the deck.
    ThemeSync,
    /// Turn the ambient screensaver on or off, or take a new original.
    Screensaver {
        #[command(subcommand)]
        action: ScreensaverAction,
    },
    /// One cycle of the ambient screensaver; the station service runs it.
    #[command(hide = true)]
    ScreensaverTick,
}

/// `fleetdeck omarchy screensaver <action>`.
#[derive(Debug, Clone, Copy, Subcommand)]
pub enum ScreensaverAction {
    /// Write the fleet summary into the screensaver file (the next tick writes).
    On,
    /// Stop writing, and put the original screensaver file back.
    Off,
    /// Keep the saved original under a new name; the next tick saves the
    /// current screensaver file as the new original.
    Rebaseline,
}
