//! The clap types of `fleetdeck desktop` (R13 P4 task B2, `omarchy-p4.md`
//! §3.3).

use clap::Subcommand;

/// `fleetdeck desktop <command>`.
#[derive(Debug, Subcommand)]
pub enum DesktopCommand {
    /// Print the fleet summary as one JSON line on each change, for a desktop bar.
    Feed {
        /// Also send a desktop notification when a session starts to need you.
        #[arg(long)]
        notify: bool,
    },
    /// Open the board in the browser, at a session when you name one.
    Open {
        /// `<station_id>/<session>`: the session to show.
        target: Option<String>,
        /// The request id to show; it needs a session.
        #[arg(long)]
        request: Option<String>,
    },
    /// Start a supervised session that diagnoses the last crash on this machine.
    Crash {
        /// Use the newest crash that systemd-coredump recorded.
        #[arg(long, required = true)]
        last: bool,
    },
    /// Write the fleet summary into the Omarchy screensaver text, once a minute.
    #[command(hide = true)]
    ScreensaverRun,
    /// The hotkeys of the wall display that is bound to this station.
    Wall {
        #[command(subcommand)]
        command: WallCommand,
    },
}

/// `fleetdeck desktop wall <command>` (R13 P4 task E).
#[derive(Debug, Subcommand)]
pub enum WallCommand {
    /// Show the next session that needs you on the wall.
    Next,
    /// Change the mode of the wall.
    Mode {
        #[command(subcommand)]
        command: WallModeCommand,
    },
    /// Show the wall window, or go back to the previous workspace.
    Show,
}

/// `fleetdeck desktop wall mode <command>`.
#[derive(Debug, Subcommand)]
pub enum WallModeCommand {
    /// Go to the next mode: mission, focus on a request, ambient.
    Next,
}
