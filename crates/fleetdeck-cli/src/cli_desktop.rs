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
}
