//! The compiled allowlist of the desktop tools (`board-gaps.md` §2.3, §2.4):
//! the executable and the package of each tool ID. No value from the wire
//! reaches this table.

use fleetdeck_proto::desktop_tools::tool;

/// One tool of `station.tools`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tool {
    /// One of [`tool::ALL`].
    pub id: &'static str,
    /// The executable name that the desktop resolver looks up.
    pub exe: &'static str,
    /// `(repository, package name)`, or `None` when no reviewed package
    /// exists.
    pub package: Option<(&'static str, &'static str)>,
}

impl Tool {
    /// The package name that must own the executable: the mapped package,
    /// or the executable name for a tool without a package.
    #[must_use]
    pub fn owner_name(&self) -> &'static str {
        self.package.map_or(self.exe, |(_, name)| name)
    }
}

/// The only repositories that a package of a plan may come from.
pub const REPOS: [&str; 2] = ["core", "extra"];

/// The nine tools, in the order of [`tool::ALL`].
pub const TOOLS: [Tool; 9] = [
    Tool {
        id: tool::WTYPE,
        exe: "wtype",
        package: Some(("extra", "wtype")),
    },
    Tool {
        id: tool::WF_RECORDER,
        exe: "wf-recorder",
        package: Some(("extra", "wf-recorder")),
    },
    Tool {
        id: tool::WLRCTL,
        exe: "wlrctl",
        package: None,
    },
    Tool {
        id: tool::GRIM,
        exe: "grim",
        package: Some(("extra", "grim")),
    },
    Tool {
        id: tool::BUSCTL,
        exe: "busctl",
        package: Some(("core", "systemd")),
    },
    Tool {
        id: tool::JQ,
        exe: "jq",
        package: Some(("extra", "jq")),
    },
    Tool {
        id: tool::HYPRLAND,
        exe: "Hyprland",
        package: Some(("extra", "hyprland")),
    },
    Tool {
        id: tool::CHROMIUM,
        exe: "chromium",
        package: Some(("extra", "chromium")),
    },
    Tool {
        id: tool::NOTIFY_SEND,
        exe: "notify-send",
        package: Some(("extra", "libnotify")),
    },
];

/// The repository-qualified package target `<repo>/<name>`, or `None`
/// when the tool has no package.
#[must_use]
pub fn target(tool: &Tool) -> Option<String> {
    tool.package.map(|(repo, name)| format!("{repo}/{name}"))
}

/// The tool with the ID `id`.
#[must_use]
pub fn by_id(id: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|t| t.id == id)
}
