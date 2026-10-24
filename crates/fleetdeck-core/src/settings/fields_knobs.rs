//! The field entries of the owner knobs (knobs design sections 2 to 4). Every knob is a
//! number with a range; `fields.rs` appends these lists to its tables.

use fleetdeck_proto::settings::Apply::{Live, Restart};
use fleetdeck_proto::settings::FieldKind as K;
use fleetdeck_proto::settings::FieldMeta;

use super::fields::field;

/// The knob fields of `deck.toml`.
pub(super) fn deck_knob_fields() -> Vec<FieldMeta> {
    [
        field(
            "security.min_password_chars",
            "Shortest owner password (characters)",
            K::Int,
            Live,
            "The fewest characters that a new owner password must have. Default 12.",
        )
        .range(8, 128)
        .danger(),
        field(
            "security.login_idle_days",
            "Login idle time (days)",
            K::Int,
            Live,
            "A board login ends after this many days without use. A lower value also ends the logins that are idle longer now. Default 7.",
        )
        .range(1, 90)
        .danger(),
        field(
            "security.login_max_days",
            "Longest login (days)",
            K::Int,
            Live,
            "A board login ends this many days after the sign-in. A lower value also ends the older logins now. Default 30.",
        )
        .range(1, 365)
        .danger(),
        field(
            "security.elevation_minutes",
            "Elevation time (minutes)",
            K::Int,
            Live,
            "How long a confirmed elevation lasts for one login. Default 10.",
        )
        .range(1, 60)
        .danger(),
        field(
            "security.api_token_max_days",
            "Longest API token (days)",
            K::Int,
            Live,
            "The longest lifetime of a new API token. Default 90.",
        )
        .range(1, 365)
        .danger(),
        field(
            "security.display_idle_days",
            "Display idle time (days)",
            K::Int,
            Live,
            "A paired wall display is signed out after this many days without use. Default 30.",
        )
        .range(1, 365)
        .danger(),
        field(
            "security.display_max_days",
            "Longest display pairing (days)",
            K::Int,
            Live,
            "A paired wall display must pair again after this many days. Default 90.",
        )
        .range(1, 730)
        .danger(),
        field(
            "security.invite_minutes",
            "Station invitation time (minutes)",
            K::Int,
            Live,
            "How long a new station invitation stays valid. Default 15.",
        )
        .range(5, 1440)
        .danger(),
        field(
            "security.desktop_screenshots_per_minute",
            "Agent screenshots per minute",
            K::Int,
            Live,
            "The most screenshots that one coordinator grant can take in one minute. Default 2.",
        )
        .range(1, 60)
        .danger(),
        field(
            "security.desktop_actions_per_minute",
            "Agent desktop actions per minute",
            K::Int,
            Live,
            "The most desktop actions that one coordinator grant can start in one minute. Default 30.",
        )
        .range(1, 300)
        .danger(),
        field(
            "retention.audit_days",
            "Audit history (days)",
            K::Int,
            Live,
            "Audit rows older than this are deleted. Default 90.",
        )
        .range(7, 3650)
        .danger(),
        field(
            "retention.audit_max_rows",
            "Most audit rows",
            K::Int,
            Live,
            "The oldest audit rows are deleted above this count. Default 200000.",
        )
        .range(10000, 10000000)
        .danger(),
        field(
            "limits.delivery_hours",
            "Mail and task delivery (hours)",
            K::Int,
            Live,
            "How long the deck tries to deliver mail and tasks to a station that is offline. Default 24.",
        )
        .range(1, 168),
        field(
            "limits.radar_minutes",
            "Conflict radar interval (minutes)",
            K::Int,
            Restart,
            "How often the deck asks the stations for Git conflicts between sessions. Default 5.",
        )
        .range(1, 60),
    ]
    .into_iter()
    .map(|f| f.0)
    .collect()
}

/// The knob fields of `station.toml`.
pub(super) fn station_knob_fields() -> Vec<FieldMeta> {
    [
        field(
            "limits.shell_command_minutes",
            "Agent shell command time (minutes)",
            K::Int,
            Live,
            "The longest run of one `!` command of an agent session. Default 10.",
        )
        .range(1, 240),
        field(
            "limits.setup_minutes",
            "Setup command time (minutes)",
            K::Int,
            Live,
            "The longest run of the setup command of a new session. Default 10.",
        )
        .range(1, 240),
        field(
            "limits.land_check_minutes",
            "Land check time (minutes)",
            K::Int,
            Live,
            "The longest run of a project's check command before a land. Default 10.",
        )
        .range(1, 240),
        field(
            "limits.clone_minutes",
            "Clone time (minutes)",
            K::Int,
            Live,
            "The longest run of the first clone of a project. Default 15.",
        )
        .range(1, 240),
        field(
            "limits.git_push_seconds",
            "Git push time (seconds)",
            K::Int,
            Live,
            "The longest run of one Git push. Default 60.",
        )
        .range(10, 600),
        field(
            "limits.provider_stop_grace_seconds",
            "Agent stop grace (seconds)",
            K::Int,
            Restart,
            "How long a stopping agent can finish before Fleet Deck ends it. Default 30.",
        )
        .range(5, 300),
        field(
            "limits.reconnect_max_seconds",
            "Longest reconnect wait (seconds)",
            K::Int,
            Restart,
            "The longest wait between two tries to reach the deck again. Default 300.",
        )
        .range(5, 600),
        field(
            "limits.scrollback_lines",
            "Terminal history (lines)",
            K::Int,
            Live,
            "The lines that a new terminal keeps. Default 5000.",
        )
        .range(500, 100000),
        field(
            "limits.scrollback_mib",
            "Terminal history (MiB)",
            K::Int,
            Live,
            "The bytes that a new terminal keeps. Default 8.",
        )
        .range(1, 64),
        field(
            "retention.history_days",
            "Session history (days)",
            K::Int,
            Restart,
            "Finished session history older than this is deleted. Default 90.",
        )
        .range(1, 3650)
        .danger(),
        field(
            "retention.event_days",
            "Delivered events (days)",
            K::Int,
            Restart,
            "Events that the deck has already received are deleted after this many days. Default 7.",
        )
        .range(1, 90),
        field(
            "retention.raw_log_mib",
            "Raw agent logs per session (MiB)",
            K::Int,
            Restart,
            "The raw provider log kept for each session, for debugging. It can hold prompt text. 0 turns it off. Default 16.",
        )
        .range(0, 256)
        .danger(),
        field(
            "capture.quota_gib",
            "Capture storage (GiB)",
            K::Int,
            Live,
            "The most disk space that screenshots and recordings use on this station. Default 4.",
        )
        .range(1, 1024),
        field(
            "capture.free_reserve_gib",
            "Free disk to keep (GiB)",
            K::Int,
            Live,
            "A capture is refused when it would leave less free disk than this. Default 2.",
        )
        .range(1, 100),
        field(
            "capture.recording_max_seconds",
            "Longest recording (seconds)",
            K::Int,
            Live,
            "The longest single recording on this station. A long recording that reaches 256 MiB is discarded. Default 120.",
        )
        .range(10, 3600),
    ]
    .into_iter()
    .map(|f| f.0)
    .collect()
}
