//! The one managed line of `hyprland.lua` (R13 P4 task C, decisions P4-7
//! and P4-13, `omarchy-p4.md` §3.4 step 7).
//!
//! The line is `pcall(dofile, "<absolute path of hypr.lua>") -- fleetdeck:
//! managed line`. Install appends it once, through the atomic writer, and
//! records the hash of the file before and after. A file that is missing, a
//! link, not regular or not yours is never edited and never created.
//! Uninstall removes exactly that line.

use std::path::Path;

use super::fsx::{FsError, Home};
use super::record::{HyprlandLine, sha256_hex};

/// The file, relative to the config home.
pub const FILE: &str = "hypr/hyprland.lua";
/// The text after the call that marks the line as ours.
const MARKER: &str = " -- fleetdeck: managed line";

/// The managed line for the installed `hypr.lua`. `None` when the path is
/// not UTF-8 or holds a control character (Lua cannot take it on one line).
#[must_use]
pub fn line_for(hypr_lua: &Path) -> Option<String> {
    let text = hypr_lua.to_str()?;
    if text.chars().any(char::is_control) {
        return None;
    }
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
    Some(format!("pcall(dofile, \"{escaped}\"){MARKER}"))
}

/// Where `line` sits in `bytes` as a whole line: the start and the end
/// (without its newline), and whether a `\n` follows.
fn find_line(bytes: &[u8], line: &str) -> Option<(usize, usize, bool)> {
    let want = line.as_bytes();
    let mut start = 0;
    while start <= bytes.len() {
        let end = bytes[start..]
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |p| start + p);
        if &bytes[start..end] == want {
            return Some((start, end, end < bytes.len()));
        }
        if end >= bytes.len() {
            return None;
        }
        start = end + 1;
    }
    None
}

/// What install did.
#[derive(Debug)]
pub struct Installed {
    /// The new `hyprland_line` object of the record.
    pub record: HyprlandLine,
    /// One line for the user.
    pub message: String,
    /// True when the user must add the line by hand.
    pub print_line: bool,
}

fn not_edited(why: &str) -> Installed {
    Installed {
        record: HyprlandLine::default(),
        message: format!("Hyprland: {why}; the line was not added."),
        print_line: true,
    }
}

/// Appends `line` to `hyprland.lua` when it is not there. `old` is the
/// `hyprland_line` of the record before this install.
pub fn install(home: &Home, line: &str, old: &HyprlandLine) -> Installed {
    let rel = Path::new(FILE);
    let shown = home.abs(rel);
    let data = match home.read(rel) {
        Ok(Some(data)) => data,
        Ok(None) => return not_edited(&format!("{} does not exist", shown.display())),
        Err(FsError::Unsafe(path)) => {
            return not_edited(&format!("{} is a link or is not yours", path.display()));
        }
        Err(e) => return not_edited(&format!("{}: {e}", shown.display())),
    };
    let now = sha256_hex(&data.bytes);
    if find_line(&data.bytes, line).is_some() {
        let record = if old.added {
            old.clone()
        } else {
            HyprlandLine {
                added: true,
                before_sha256: Some(now.clone()),
                after_sha256: Some(now),
                newline_added: false,
            }
        };
        return Installed {
            record,
            message: format!("Hyprland: the line is already in {}.", shown.display()),
            print_line: false,
        };
    }
    let newline_added = !data.bytes.is_empty() && !data.bytes.ends_with(b"\n");
    let mut bytes = data.bytes.clone();
    if newline_added {
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
    if let Err(e) = home.write(rel, &bytes, data.mode) {
        return not_edited(&format!("{}: {e}", shown.display()));
    }
    Installed {
        record: HyprlandLine {
            added: true,
            before_sha256: Some(now),
            after_sha256: Some(sha256_hex(&bytes)),
            newline_added,
        },
        message: format!("Hyprland: added the managed line to {}.", shown.display()),
        print_line: false,
    }
}

/// What uninstall did.
#[derive(Debug, PartialEq, Eq)]
pub enum Removal {
    /// Nothing to do: the line was not added by the install, or it is gone.
    Nothing,
    /// The line was removed and the file was as install left it.
    Removed,
    /// The line was removed; the file had other changes, which stay.
    KeptOthers,
    /// The file could not be edited.
    Failed(String),
}

/// Removes the managed line when the record says that install added it.
pub fn uninstall(home: &Home, line: &str, record: &HyprlandLine) -> Removal {
    if !record.added {
        return Removal::Nothing;
    }
    let rel = Path::new(FILE);
    let data = match home.read(rel) {
        Ok(Some(data)) => data,
        Ok(None) => return Removal::Nothing,
        Err(e) => return Removal::Failed(e.to_string()),
    };
    let Some((start, end, has_newline)) = find_line(&data.bytes, line) else {
        return Removal::Nothing;
    };
    let unchanged = record.after_sha256.as_deref() == Some(sha256_hex(&data.bytes).as_str());
    let mut from = start;
    let to = if has_newline { end + 1 } else { end };
    // The newline that install wrote before the line goes too, when the
    // line is the last one of the file (so no text of the user joins).
    if record.newline_added && to == data.bytes.len() && from > 0 && data.bytes[from - 1] == b'\n' {
        from -= 1;
    }
    let mut bytes = data.bytes[..from].to_vec();
    bytes.extend_from_slice(&data.bytes[to..]);
    match home.write(rel, &bytes, data.mode) {
        Ok(()) if unchanged => Removal::Removed,
        Ok(()) => Removal::KeptOthers,
        Err(e) => Removal::Failed(e.to_string()),
    }
}

/// Whether the managed line is in the file now.
#[must_use]
pub fn present(home: &Home, line: &str) -> bool {
    matches!(
        home.read(Path::new(FILE)),
        Ok(Some(data)) if find_line(&data.bytes, line).is_some()
    )
}
