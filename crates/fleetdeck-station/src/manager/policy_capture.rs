//! The recording length of the station policy (knobs K4, design
//! `docs/v2/design/knobs.md` section 3, "Recording length"). It is a live
//! value: `capture.recording_max_seconds` of `station.toml`, read once when a
//! recording starts and reported in `desktop.info`. The capture storage
//! (`capture.quota_gib`, `capture.free_reserve_gib`, knobs K3b) is live too:
//! a capture or a recording reads it when it starts.

use super::policy::Policy;

/// One GiB in bytes.
const GIB: u64 = 1024 * 1024 * 1024;

/// `n` GiB in bytes.
#[must_use]
pub(super) fn gib(n: u32) -> u64 {
    u64::from(n) * GIB
}

impl Policy {
    /// The most that the captures and recordings of this station may use on
    /// disk, in bytes (`capture.quota_gib`).
    #[must_use]
    pub fn capture_quota_bytes(&self) -> u64 {
        self.capture_quota_bytes
    }

    /// The free disk space that a capture must leave, in bytes
    /// (`capture.free_reserve_gib`).
    #[must_use]
    pub fn capture_free_reserve_bytes(&self) -> u64 {
        self.capture_free_reserve_bytes
    }

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
    fn the_default_config_gives_4_gib_of_quota_and_2_gib_of_reserve() {
        let root = tempfile::tempdir().unwrap();
        let policy = Policy::from_config(&test_config(root.path(), ""));
        assert_eq!(policy.capture_quota_bytes(), 4 * GIB);
        assert_eq!(policy.capture_free_reserve_bytes(), 2 * GIB);
    }

    #[test]
    fn the_quota_and_the_reserve_are_live_values() {
        let root = tempfile::tempdir().unwrap();
        let policy = Policy::from_config(&test_config(
            root.path(),
            "[capture]\nquota_gib = 10\nfree_reserve_gib = 5\n",
        ));
        assert_eq!(policy.capture_quota_bytes(), 10 * GIB);
        assert_eq!(policy.capture_free_reserve_bytes(), 5 * GIB);
        let next = policy.reconfigured(&test_config(
            root.path(),
            "[capture]\nquota_gib = 1\nfree_reserve_gib = 1\n",
        ));
        assert_eq!(next.capture_quota_bytes(), GIB);
        assert_eq!(next.capture_free_reserve_bytes(), GIB);
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
