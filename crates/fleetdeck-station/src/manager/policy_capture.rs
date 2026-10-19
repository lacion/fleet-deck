//! The recording length of the station policy (knobs K4, design
//! `docs/v2/design/knobs.md` section 3, "Recording length"). It is a live
//! value: `capture.recording_max_seconds` of `station.toml`, read once when a
//! recording starts and reported in `desktop.info`.

use super::policy::Policy;

impl Policy {
    /// The longest recording that this station allows, in seconds.
    #[must_use]
    pub fn recording_max_seconds(&self) -> u32 {
        self.recording_max_seconds
    }

    /// The same policy with another longest recording (tests).
    #[must_use]
    pub fn with_recording_max_seconds(mut self, seconds: u32) -> Self {
        self.recording_max_seconds = seconds;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::policy_hub::test_config;

    #[test]
    fn the_default_config_gives_120_seconds() {
        let root = tempfile::tempdir().unwrap();
        let policy = Policy::from_config(&test_config(root.path(), ""));
        assert_eq!(policy.recording_max_seconds(), 120);
    }

    #[test]
    fn the_config_value_and_a_reconfiguration_set_the_length() {
        let root = tempfile::tempdir().unwrap();
        let policy = Policy::from_config(&test_config(
            root.path(),
            "[capture]\nrecording_max_seconds = 600\n",
        ));
        assert_eq!(policy.recording_max_seconds(), 600);
        let next = policy.reconfigured(&test_config(
            root.path(),
            "[capture]\nrecording_max_seconds = 3600\n",
        ));
        assert_eq!(next.recording_max_seconds(), 3600);
    }
}
