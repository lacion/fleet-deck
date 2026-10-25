//! The pure checks of the run (design `omarchy-p8.md` 3.2): what an answer of
//! the station must match, and the reading of `omarchy status --json`.

use serde::Deserialize;

use super::effects::StationView;
use super::release::Manifest;

/// The build and the binary hash of the answer are the ones of the manifest.
pub fn build_matches(view: &StationView, manifest: &Manifest) -> bool {
    view.build.release
        && view.build.version == manifest.release_version
        && view.build.commit == manifest.commit
        && view.binary_sha256 == manifest.binary_sha256
}

#[derive(Debug, Deserialize)]
struct FileEntry {
    state: String,
}

/// The answer of `omarchy status --json` (the fields that P8 reads).
#[derive(Debug, Deserialize)]
pub struct IntegrationStatus {
    installed: bool,
    files: Vec<FileEntry>,
    plugin_enabled: bool,
    hyprland_line: String,
}

impl IntegrationStatus {
    /// `None` when the bytes are not the JSON object of the status.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }

    /// A record exists and no recorded file is `changed` or `missing`.
    pub fn files_clean(&self) -> bool {
        self.installed && self.files.iter().all(|f| f.state == "ok")
    }

    pub fn plugin_enabled(&self) -> bool {
        self.plugin_enabled
    }

    /// The managed line is not in `hyprland.lua`.
    pub fn hyprland_line_missing(&self) -> bool {
        self.hyprland_line == "missing"
    }

    /// Files clean and the plugin enabled.
    pub fn healthy(&self) -> bool {
        self.files_clean() && self.plugin_enabled
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::provision::fake::status_json;

    fn parsed(
        code: i32,
        installed: bool,
        files: &[&str],
        plugin: bool,
        line: &str,
    ) -> Option<IntegrationStatus> {
        IntegrationStatus::parse(&status_json(code, installed, files, plugin, line).1)
    }

    #[test]
    fn the_status_is_read_from_the_json_of_the_real_command() {
        let status = parsed(0, true, &["ok", "ok"], true, "ok").unwrap();
        assert!(status.files_clean() && status.plugin_enabled() && status.healthy());
        assert!(!status.hyprland_line_missing());
        let status = parsed(0, true, &["ok"], false, "missing").unwrap();
        assert!(status.files_clean() && !status.healthy());
        assert!(status.hyprland_line_missing());
        for files in [&["ok", "changed"][..], &["missing"][..]] {
            let status = parsed(1, true, files, true, "ok").unwrap();
            assert!(!status.files_clean() && !status.healthy());
        }
        assert!(
            !parsed(1, false, &[], false, "missing")
                .unwrap()
                .files_clean()
        );
    }

    #[test]
    fn text_that_is_not_the_status_is_none() {
        for bytes in [&b""[..], b"not json", b"{}", b"[]", b"{\"installed\":true}"] {
            assert!(IntegrationStatus::parse(bytes).is_none());
        }
    }
}
