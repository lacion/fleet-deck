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
    /// The coordinator desktop tools (R13 P6 §2.1; fleet control, OD-3).
    Desktop,
    /// The coordinator screenshot tools (R13 P6 §2.1; fleet control, OD-3).
    Capture,
    /// Station-internal (R13 P5b-2 §13.5): the grant has the explicit
    /// recording allowance. The start adds it to the binding classes only
    /// when the spec has `allow_recording` and the `capture` class.
    /// [`ToolClass::parse`] never returns it, so a deck grant cannot name it.
    Recording,
    /// The coordinator input tools (R13 P7 §3.1; fleet control, mutating).
    /// It does not imply another class.
    Input,
    /// Station-internal (R13 P7 §3.2): the grant has the explicit input
    /// allowance. The start adds it to the binding classes only when the spec
    /// has `allow_input`, the scope `own_windows`, the classes `desktop`,
    /// `capture` and `input`, and the deck link negotiated
    /// `desktop_input_mcp.v1`. [`ToolClass::parse`] never returns it, so a
    /// deck grant cannot name it.
    InputEnabled,
    /// The session's own tools (`publish_artifact`, `set_focus`,
    /// `ask_coordinator`; DL-165). Every binding has it; no grant names
    /// it ([`ToolClass::parse`] never returns it).
    Own,
}

impl ToolClass {
    /// The class name: the inverse of [`Self::parse`] for a grant class.
    /// `own` is no grant class and is never stored; `recording` is stored
    /// but is no grant class (see [`Self::from_stored`]); so is
    /// `input_enabled`.
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
            Self::Desktop => "desktop",
            Self::Capture => "capture",
            Self::Recording => "recording",
            Self::Input => "input",
            Self::InputEnabled => "input_enabled",
            Self::Own => "own",
        }
    }

    /// The class of a grant class name (`read`, `mail`, `drive`, `spawn`,
    /// `inspect`, `manage`, `answer`, `desktop`, `capture`, `input`). Unknown names
    /// fail closed.
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
            "desktop" => Self::Desktop,
            "capture" => Self::Capture,
            "input" => Self::Input,
            _ => return None,
        })
    }

    /// The class of a stored name: a grant class, or the station-internal
    /// `recording` (R13 P5b-2) or `input_enabled` (R13 P7). `own` is never
    /// stored and fails closed.
    #[must_use]
    pub fn from_stored(name: &str) -> Option<Self> {
        match name {
            "recording" => Some(Self::Recording),
            "input_enabled" => Some(Self::InputEnabled),
            other => Self::parse(other),
        }
    }

    /// The fleet-control classes (ADR-008 §2, R13 P6 OD-3).
    #[must_use]
    pub fn is_fleet_control(self) -> bool {
        matches!(
            self,
            Self::Inspect
                | Self::Manage
                | Self::Answer
                | Self::Desktop
                | Self::Capture
                | Self::Input
        )
    }

    /// The classes of a grant that change something (ADR-008 §4.2 rule 3).
    #[must_use]
    pub fn is_mutating(self) -> bool {
        matches!(
            self,
            Self::Mail
                | Self::Drive
                | Self::Spawn
                | Self::Manage
                | Self::Answer
                | Self::Desktop
                | Self::Capture
                | Self::Input
        )
    }
}

#[cfg(test)]
mod tests {
    use super::ToolClass;

    #[test]
    fn the_new_classes_have_their_names_and_parse_back() {
        for (class, name) in [
            (ToolClass::Desktop, "desktop"),
            (ToolClass::Capture, "capture"),
            (ToolClass::Input, "input"),
        ] {
            assert_eq!(class.name(), name);
            assert_eq!(ToolClass::parse(name), Some(class));
            assert!(class.is_fleet_control());
            assert!(class.is_mutating());
        }
    }

    #[test]
    fn every_other_class_keeps_its_rules_and_unknown_names_fail_closed() {
        for (class, fleet, mutating) in [
            (ToolClass::Read, false, false),
            (ToolClass::Mail, false, true),
            (ToolClass::Drive, false, true),
            (ToolClass::Spawn, false, true),
            (ToolClass::Inspect, true, false),
            (ToolClass::Manage, true, true),
            (ToolClass::Answer, true, true),
            (ToolClass::Own, false, false),
        ] {
            assert_eq!(class.is_fleet_control(), fleet, "{class:?}");
            assert_eq!(class.is_mutating(), mutating, "{class:?}");
        }
        for name in [
            "input_enabled",
            "InputEnabled",
            "Input",
            "Desktop",
            "desktops",
            "own",
            "",
            "screenshot",
        ] {
            assert_eq!(ToolClass::parse(name), None, "{name}");
        }
    }

    #[test]
    fn the_recording_class_is_internal_and_gives_no_fleet_control() {
        let class = ToolClass::Recording;
        assert_eq!(class.name(), "recording");
        // A grant never names it; only the stored form reads it back.
        assert_eq!(ToolClass::parse("recording"), None);
        assert_eq!(ToolClass::from_stored("recording"), Some(class));
        assert_eq!(ToolClass::from_stored("capture"), Some(ToolClass::Capture));
        assert_eq!(ToolClass::from_stored("own"), None);
        assert!(!class.is_fleet_control());
        assert!(!class.is_mutating());
    }

    #[test]
    fn the_input_enabled_class_is_internal_and_gives_no_fleet_control() {
        let class = ToolClass::InputEnabled;
        assert_eq!(class.name(), "input_enabled");
        // A grant never names it; only the stored form reads it back.
        assert_eq!(ToolClass::parse("input_enabled"), None);
        assert_eq!(ToolClass::parse("InputEnabled"), None);
        assert_eq!(ToolClass::from_stored("input_enabled"), Some(class));
        assert_eq!(ToolClass::from_stored("InputEnabled"), None);
        assert_eq!(ToolClass::from_stored("input"), Some(ToolClass::Input));
        assert!(!class.is_fleet_control());
        assert!(!class.is_mutating());
    }
}
