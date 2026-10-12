//! The human shell (PLAN §6.8, D10; M3 item 6-11, DL-404).
//!
//! The owner opens a PTY shell on a station from the board. `shell.open`,
//! `shell.resize` and `shell.close` are mutating link methods with a
//! [`ShellEnvelope`] (audited, with a `command_id`); `shell.list` reads the
//! shells; `shell.attach` returns a stream token for the data channel. The
//! bytes of a shell (output and keystrokes) travel only on that data
//! stream, never on the control channel (A7). Keystrokes are not audited.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{CommandId, IncarnationId};

/// The most lines that the scrollback of a shell keeps.
pub const SCROLLBACK_LINES: usize = 5_000;
/// The most bytes that the scrollback of a shell keeps (8 MiB).
pub const SCROLLBACK_BYTES: usize = 8 * 1024 * 1024;
/// The longest shell id, in characters.
pub const MAX_SHELL_ID_CHARS: usize = 64;
/// The largest terminal size in columns or rows.
pub const MAX_TERMINAL_CELLS: u16 = 1_000;
/// The data stream purpose prefix of a shell (`shell:<shell_id>`).
pub const STREAM_PURPOSE: &str = "shell:";

/// One shell change (tag `op`). The deck chooses the shell id, so a resent
/// `shell.open` names the same shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "shell/")]
pub enum ShellChange {
    /// Starts the owner's login shell in `cwd` (inside the allowed roots).
    Open {
        shell_id: String,
        cwd: String,
        cols: u16,
        rows: u16,
    },
    Resize {
        shell_id: String,
        cols: u16,
        rows: u16,
    },
    /// Ends the shell (SIGHUP to its process group) and drops its scrollback.
    Close { shell_id: String },
}

impl ShellChange {
    /// The link method of the change.
    #[must_use]
    pub fn method(&self) -> &'static str {
        match self {
            Self::Open { .. } => "shell.open",
            Self::Resize { .. } => "shell.resize",
            Self::Close { .. } => "shell.close",
        }
    }

    #[must_use]
    pub fn shell_id(&self) -> &str {
        match self {
            Self::Open { shell_id, .. }
            | Self::Resize { shell_id, .. }
            | Self::Close { shell_id } => shell_id,
        }
    }

    /// The field checks that need no station state.
    ///
    /// # Errors
    ///
    /// The reason text when a field is out of range.
    pub fn check(&self) -> Result<(), &'static str> {
        let id = self.shell_id();
        if id.is_empty()
            || id.chars().count() > MAX_SHELL_ID_CHARS
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err("a shell id has 1 to 64 characters: letters, digits, `-`, `_`");
        }
        match self {
            Self::Open { cols, rows, .. } | Self::Resize { cols, rows, .. } => {
                if *cols == 0
                    || *rows == 0
                    || *cols > MAX_TERMINAL_CELLS
                    || *rows > MAX_TERMINAL_CELLS
                {
                    return Err("the terminal size is 1 to 1000 columns and rows");
                }
                Ok(())
            }
            Self::Close { .. } => Ok(()),
        }
    }
}

/// The envelope of `shell.open`, `shell.resize` and `shell.close`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "shell/")]
pub struct ShellEnvelope {
    pub command_id: CommandId,
    pub target_incarnation: IncarnationId,
    pub retry_until: i64,
    pub change: ShellChange,
}

/// The state of a shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "shell/")]
pub enum ShellState {
    Running,
    /// The shell process ended; the scrollback stays until `shell.close`.
    Exited,
}

/// One shell of a station (`shell.list`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "shell/")]
pub struct ShellInfo {
    pub shell_id: String,
    pub cwd: String,
    pub state: ShellState,
    pub cols: u16,
    pub rows: u16,
    /// Unix ms.
    #[ts(type = "number")]
    pub created_at: i64,
    /// The exit code once the shell ended (`None` for a signal).
    pub exit_code: Option<u32>,
}

/// The params of `shell.attach`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "shell/")]
pub struct ShellAttach {
    pub shell_id: String,
}

/// The answer of `shell.attach`: open a data stream with `stream_token`;
/// the station sends the scrollback, then the live bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellAttached {
    pub stream_token: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(id: &str, cols: u16, rows: u16) -> ShellChange {
        ShellChange::Open {
            shell_id: id.to_owned(),
            cwd: "/w".to_owned(),
            cols,
            rows,
        }
    }

    #[test]
    fn shell_changes_check_their_ids_and_terminal_sizes() {
        assert!(open("sh-1", 80, 24).check().is_ok());
        assert!(open("", 80, 24).check().is_err());
        assert!(open("a/b", 80, 24).check().is_err());
        assert!(open(&"x".repeat(65), 80, 24).check().is_err());
        assert!(open("sh", 0, 24).check().is_err());
        assert!(open("sh", 80, 1001).check().is_err());
        let close: ShellChange =
            serde_json::from_value(serde_json::json!({"op": "close", "shell_id": "sh"}))
                .expect("close");
        assert_eq!(close.method(), "shell.close");
        let unknown = serde_json::from_value::<ShellChange>(
            serde_json::json!({"op": "close", "shell_id": "sh", "x": 1}),
        );
        assert!(unknown.is_err(), "unknown fields fail closed");
    }
}
