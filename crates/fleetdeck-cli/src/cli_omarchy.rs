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
