//! The files that `fleetdeck omarchy install` writes (R13 P4 task C,
//! `omarchy-p4.md` §3.4, §3.5 and §3.7). The text is embedded with
//! `include_str!` from `assets/omarchy/`.

use serde::{Deserialize, Serialize};

/// The folder that a path is relative to, below the config home.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Base {
    /// `<config home>/omarchy`.
    Omarchy,
    /// `<config home>/fleetdeck`.
    Fleetdeck,
}

impl Base {
    /// The folder name below the config home.
    #[must_use]
    pub const fn dir(self) -> &'static str {
        match self {
            Self::Omarchy => "omarchy",
            Self::Fleetdeck => "fleetdeck",
        }
    }
}

/// One file of the install.
#[derive(Debug, Clone, Copy)]
pub struct Asset {
    pub base: Base,
    /// Relative to the base.
    pub path: &'static str,
    /// The permission bits.
    pub mode: u32,
    pub text: &'static str,
    /// Written only when no file exists (the menu file of the user).
    pub only_if_absent: bool,
}

/// The path of the menu file, relative to the Omarchy base.
pub const MENU_PATH: &str = "extensions/omarchy-menu.jsonc";
/// The path of the Hyprland file that the installed `hypr.lua` is loaded from.
pub const HYPR_PATH: &str = "omarchy/hypr.lua";

/// The files, in the order of the install.
pub const FILES: &[Asset] = &[
    Asset {
        base: Base::Omarchy,
        path: "plugins/fleetdeck.fleet/manifest.json",
        mode: 0o644,
        text: include_str!("../../assets/omarchy/plugin/manifest.json"),
        only_if_absent: false,
    },
    Asset {
        base: Base::Omarchy,
        path: "plugins/fleetdeck.fleet/Widget.qml",
        mode: 0o644,
        text: include_str!("../../assets/omarchy/plugin/Widget.qml"),
        only_if_absent: false,
    },
    Asset {
        base: Base::Omarchy,
        path: "plugins/fleetdeck.fleet/Panel.qml",
        mode: 0o644,
        text: include_str!("../../assets/omarchy/plugin/Panel.qml"),
        only_if_absent: false,
    },
    Asset {
        base: Base::Omarchy,
        path: "plugins/fleetdeck.fleet/Service.qml",
        mode: 0o644,
        text: include_str!("../../assets/omarchy/plugin/Service.qml"),
        only_if_absent: false,
    },
    Asset {
        base: Base::Omarchy,
        path: "plugins/fleetdeck.fleet/Model.js",
        mode: 0o644,
        text: include_str!("../../assets/omarchy/plugin/Model.js"),
        only_if_absent: false,
    },
    Asset {
        base: Base::Omarchy,
        path: "plugins/fleetdeck.fleet/i18n/en.json",
        mode: 0o644,
        text: include_str!("../../assets/omarchy/plugin/i18n/en.json"),
        only_if_absent: false,
    },
    Asset {
        base: Base::Omarchy,
        path: "hooks/post-boot.d/fleetdeck",
        mode: 0o755,
        text: include_str!("../../assets/omarchy/hooks/post-boot"),
        only_if_absent: false,
    },
    Asset {
        base: Base::Omarchy,
        path: "hooks/post-update.d/fleetdeck",
        mode: 0o755,
        text: include_str!("../../assets/omarchy/hooks/post-update"),
        only_if_absent: false,
    },
    Asset {
        base: Base::Omarchy,
        path: "hooks/theme-set.d/fleetdeck",
        mode: 0o755,
        text: include_str!("../../assets/omarchy/hooks/theme-set"),
        only_if_absent: false,
    },
    Asset {
        base: Base::Omarchy,
        path: "themed/fleetdeck.json.tpl",
        mode: 0o644,
        text: include_str!("../../assets/omarchy/themed/fleetdeck.json.tpl"),
        only_if_absent: false,
    },
    Asset {
        base: Base::Fleetdeck,
        path: HYPR_PATH,
        mode: 0o644,
        text: include_str!("../../assets/omarchy/hypr.lua"),
        only_if_absent: false,
    },
    Asset {
        base: Base::Omarchy,
        path: MENU_PATH,
        mode: 0o644,
        text: include_str!("../../assets/omarchy/omarchy-menu.jsonc"),
        only_if_absent: true,
    },
];

/// The menu rows to add by hand: the lines of the menu asset between the
/// opening and the closing brace.
#[must_use]
pub fn menu_rows() -> Vec<&'static str> {
    let Some(asset) = FILES.iter().find(|a| a.path == MENU_PATH) else {
        return Vec::new();
    };
    asset
        .text
        .lines()
        .filter(|line| line.starts_with("  \""))
        .collect()
}

#[cfg(test)]
#[path = "assets_tests.rs"]
mod tests;
