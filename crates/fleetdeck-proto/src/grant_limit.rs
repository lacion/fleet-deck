//! The "no limit" value of a grant cap (B27 §3.3).

/// The spawn cap, the answer cap or the idle time (as `idle_ttl_ms` of a
/// board request) of a grant that has no limit: the largest `u32`.
pub const NO_LIMIT: u32 = u32::MAX;
