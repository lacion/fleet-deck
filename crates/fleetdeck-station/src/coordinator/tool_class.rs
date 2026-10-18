//! The tool class of a coordinator grant (ADR-006 §4, §6.3). Moved from
//! `bindings.rs` for the module size rule (R13 P6 F0); `bindings.rs`
//! re-exports [`ToolClass`].

/// A tool class of a grant (ADR-006 §4, §6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolClass {
    Read,
    Mail,
    Drive,
    Spawn,
    /// Every read query that ADR-008 adds (fleet control).
    Inspect,
    /// Resume, rename, set mode, move to a project, start in a project
    /// (fleet control).
    Manage,
    /// Answer approvals and questions (fleet control).
    Answer,
    /// The session's own tools (`publish_artifact`, `set_focus`,
    /// `ask_coordinator`; DL-165). Every binding has it; no grant names
    /// it ([`ToolClass::parse`] never returns it).
    Own,
}

impl ToolClass {
    /// The grant class name of a class: the inverse of [`Self::parse`]
    /// (`own` is no grant class and is never stored).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Mail => "mail",
            Self::Drive => "drive",
            Self::Spawn => "spawn",
            Self::Inspect => "inspect",
            Self::Manage => "manage",
            Self::Answer => "answer",
            Self::Own => "own",
        }
    }

    /// The class of a grant class name (`read`, `mail`, `drive`, `spawn`,
    /// `inspect`, `manage`, `answer`). Unknown names fail closed.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "read" => Self::Read,
            "mail" => Self::Mail,
            "drive" => Self::Drive,
            "spawn" => Self::Spawn,
            "inspect" => Self::Inspect,
            "manage" => Self::Manage,
            "answer" => Self::Answer,
            _ => return None,
        })
    }

    /// The fleet-control classes (ADR-008 §2).
    #[must_use]
    pub fn is_fleet_control(self) -> bool {
        matches!(self, Self::Inspect | Self::Manage | Self::Answer)
    }

    /// The classes of a grant that change something (ADR-008 §4.2 rule 3).
    #[must_use]
    pub fn is_mutating(self) -> bool {
        matches!(
            self,
            Self::Mail | Self::Drive | Self::Spawn | Self::Manage | Self::Answer
        )
    }
}
