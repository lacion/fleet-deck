//! The deck side of the dispatch authority of `desktop.act` (R13 §5E item 1,
//! §5F, §5G item 1).
//!
//! STEP 1 STUB. The module is declared so that the P3b relay task owns this
//! file and never edits `link/mod.rs`. The relay task builds here the
//! authority for each action with the shared helpers of
//! `fleetdeck_link::desktop_authority` (`action_hash`, `issue`) and the
//! constants of `fleetdeck_proto::desktop_authority`: the deadline is the
//! deck clock plus 10 s, the `desktop_epoch` is
//! `Control::params().peer_desktop_epoch` of the live link, and the station
//! node fingerprint is the pinned key of that link. The deck stops issuing
//! authorities at once when `desktop_control` turns off.
