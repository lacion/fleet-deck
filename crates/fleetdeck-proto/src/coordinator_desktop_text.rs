//! The codes, texts, limits and deadlines of the P6 coordinator desktop tools
//! (`docs/v2/design/omarchy-p6.md` §8, §9, §11). The texts are fixed: an
//! agent-visible error never carries supplied text, a path or a wire message.

/// The P6 error codes of §11.1, and `desktop_unsupported` of §11.2.
pub mod code {
    pub const NOT_BOUND: &str = "not_bound";
    pub const NOT_GRANTED: &str = "not_granted";
    pub const BINDING_STALE: &str = "binding_stale";
    pub const ELEVATION_REQUIRED: &str = "elevation_required";
    pub const UNSUPPORTED: &str = "unsupported";
    pub const INCARNATION_MISMATCH: &str = "incarnation_mismatch";
    pub const EXPIRED: &str = "expired";
    pub const INVALID_PARAMS: &str = "invalid_params";
    pub const REQUEST_ID_CONFLICT: &str = "request_id_conflict";
    pub const DESKTOP_CAP_REACHED: &str = "desktop_cap_reached";
    pub const CAPTURE_CAP_REACHED: &str = "capture_cap_reached";
    pub const IMAGE_BUDGET_REACHED: &str = "image_budget_reached";
    pub const REQUEST_BUDGET_REACHED: &str = "request_budget_reached";
    pub const RATE_LIMITED: &str = "rate_limited";
    pub const BUSY: &str = "busy";
    pub const AUTHORITY_LOST: &str = "authority_lost";
    pub const AUTHORITY_UNKNOWN: &str = "authority_unknown";
    pub const FLEET_CONTROL_OFF: &str = "fleet_control_off";
    pub const DESKTOP_RESULT_TOO_LARGE: &str = "desktop_result_too_large";
    pub const IMAGE_TOO_LARGE: &str = "image_too_large";
    pub const IMAGE_INVALID: &str = "image_invalid";
    pub const UNKNOWN_ARTIFACT: &str = "unknown_artifact";
    pub const DESKTOP_WINDOW_NOT_FOUND: &str = "desktop_window_not_found";
    pub const DESKTOP_DISPLAY_NOT_FOUND: &str = "desktop_display_not_found";
    pub const DESKTOP_REFERENCE_NOT_GRANTED: &str = "desktop_reference_not_granted";
    pub const DESKTOP_READ_TIMEOUT: &str = "desktop_read_timeout";
    /// §11.2: the station build has no desktop control (also the stub answer
    /// of a build that has the P6 methods but no P6 behavior).
    pub const DESKTOP_UNSUPPORTED: &str = "desktop_unsupported";
    /// §11.2: the station refused the desktop action.
    pub const DESKTOP_FORBIDDEN: &str = "desktop_forbidden";
    /// §11.2: the desktop action is not valid.
    pub const INVALID_DESKTOP_ACTION: &str = "invalid_desktop_action";
    /// §11.2: the target of the desktop action was not found.
    pub const DESKTOP_NOT_FOUND: &str = "desktop_not_found";
    /// §11.2: the desktop action conflicts with the current state.
    pub const DESKTOP_CONFLICT: &str = "desktop_conflict";
    /// §11.2: the desktop action failed.
    pub const DESKTOP_FAILED: &str = "desktop_failed";
    /// §11.2: the station is busy.
    pub const DESKTOP_OVERLOADED: &str = "desktop_overloaded";
    /// §11.2: the station did not answer in time.
    pub const DESKTOP_TIMEOUT: &str = "desktop_timeout";
    /// §11.2: the station is not connected.
    pub const STATION_UNREACHABLE: &str = "station_unreachable";
    /// §11.2: desktop control is off on this deck.
    pub const DESKTOP_CONTROL_OFF: &str = "desktop_control_off";
    /// R13 P5b §16: the grant has no recording allowance.
    pub const RECORDING_NOT_ALLOWED: &str = "recording_not_allowed";
    /// R13 P5b §16: the grant used its recording count.
    pub const RECORDING_CAP_REACHED: &str = "recording_cap_reached";
    /// R13 P5b §16: the grant used its recording seconds.
    pub const RECORDING_SECONDS_REACHED: &str = "recording_seconds_reached";
    /// R13 P7 §13: the grant has the input class but no input allowance.
    pub const INPUT_NOT_ALLOWED: &str = "input_not_allowed";
    /// R13 P7 §13: an owner grant request names an unsupported input scope.
    pub const INPUT_SCOPE_UNSUPPORTED: &str = "input_scope_unsupported";
    /// R13 P7 §13: `wtype` is not installed on the target.
    pub const INPUT_TOOL_MISSING: &str = "input_tool_missing";
    /// R13 P7 §13: the target desktop cannot use `wtype`.
    pub const INPUT_TOOL_UNSUPPORTED: &str = "input_tool_unsupported";
    /// R13 P7 §13: the target window is a board or wall window.
    pub const INPUT_BOARD_FORBIDDEN: &str = "input_board_forbidden";
    /// R13 P7 §13: the grant used its input action count.
    pub const INPUT_CAP_REACHED: &str = "input_cap_reached";
    /// R13 P7 §13: the grant used its typed-character count.
    pub const INPUT_CHARACTERS_REACHED: &str = "input_characters_reached";
    /// R13 P7 §13: the pointer destination is not inside the preview.
    pub const INPUT_DESTINATION_UNAVAILABLE: &str = "input_destination_unavailable";
    /// R13 P7 §13: the focus of the preview could not be verified.
    pub const INPUT_FOCUS_UNKNOWN: &str = "input_focus_unknown";
    /// R13 P7 §13: the preview lost focus.
    pub const FOCUS_CHANGED: &str = "focus_changed";
    /// R13 P7 §13: the input was interrupted.
    pub const INPUT_INTERRUPTED: &str = "input_interrupted";
    /// R13 P7 §7.6, §13: the board API answer (401) that signs a browser out
    /// because an agent typed on its machine.
    pub const SIGNED_OUT_FOR_INPUT: &str = "signed_out_for_input";
    /// R13 P7 §7.6: the board API answer (403) to a sign-in from an address
    /// where an agent is typing.
    pub const SIGN_IN_BLOCKED_FOR_INPUT: &str = "sign_in_blocked_for_input";
    /// R13 P7 §13: the input tool failed.
    pub const INPUT_FAILED: &str = "input_failed";
    /// R13 P7 §13: the input did not finish in time.
    pub const INPUT_TIMEOUT: &str = "input_timeout";
}

/// The exact text of an error code of §11.1, or of a relay code of §11.2.
/// `None` for any other code.
#[must_use]
pub fn text(code: &str) -> Option<&'static str> {
    Some(match code {
        code::NOT_BOUND => "This coordinator is not bound to a provider session.",
        code::NOT_GRANTED => "This grant does not allow this desktop operation.",
        code::BINDING_STALE => {
            "The coordinator binding has changed. Start a new authorized request."
        }
        code::ELEVATION_REQUIRED => "Owner elevation is required for desktop or capture authority.",
        code::UNSUPPORTED => "The selected peers do not support this desktop grant.",
        code::INCARNATION_MISMATCH => {
            "The deck identity has changed. Reconnect before trying again."
        }
        code::EXPIRED => "The desktop request expired before it was accepted.",
        code::INVALID_PARAMS => "The desktop tool arguments are not valid.",
        code::REQUEST_ID_CONFLICT => "This request ID was used for another desktop request.",
        code::DESKTOP_CAP_REACHED => "This grant has used its desktop action limit.",
        code::CAPTURE_CAP_REACHED => "This grant has used its screenshot limit.",
        code::IMAGE_BUDGET_REACHED => "This grant has used its screenshot image-read budget.",
        code::REQUEST_BUDGET_REACHED => "This grant has used its desktop request limit.",
        code::RATE_LIMITED => "This grant's desktop rate limit has been reached. Try again later.",
        code::BUSY => "The desktop operation cannot start because its capacity limit is in use.",
        code::AUTHORITY_LOST => {
            "Desktop authority was withdrawn before this operation could continue."
        }
        code::AUTHORITY_UNKNOWN => {
            "Desktop authority could not be checked. No further desktop work was started."
        }
        code::FLEET_CONTROL_OFF => "Fleet control is off on this deck.",
        code::DESKTOP_RESULT_TOO_LARGE => {
            "The desktop information exceeds the tool response limit."
        }
        code::IMAGE_TOO_LARGE => {
            "The screenshot exceeds the 700 KiB MCP image limit. Open the artifact on the board."
        }
        code::IMAGE_INVALID => "The screenshot artifact could not be verified.",
        code::UNKNOWN_ARTIFACT => "No such capture artifact is available to this grant.",
        code::DESKTOP_WINDOW_NOT_FOUND => "No window opened by this grant matches that handle.",
        code::DESKTOP_DISPLAY_NOT_FOUND => "No wall available to this grant matches that display.",
        code::DESKTOP_REFERENCE_NOT_GRANTED => "This grant cannot show that reference on the wall.",
        code::DESKTOP_READ_TIMEOUT => "The desktop data did not finish loading in time.",
        code::DESKTOP_UNSUPPORTED => "This station's build has no desktop control.",
        code::DESKTOP_FORBIDDEN => "The station refused the desktop action.",
        code::INVALID_DESKTOP_ACTION => "The desktop action is not valid.",
        code::DESKTOP_NOT_FOUND => "The target of the desktop action was not found.",
        code::DESKTOP_CONFLICT => "The desktop action conflicts with the current state.",
        code::DESKTOP_FAILED => "The desktop action failed.",
        code::DESKTOP_OVERLOADED => "The station is busy; try again later.",
        code::DESKTOP_TIMEOUT => "The station did not answer in time; the action may have run.",
        code::STATION_UNREACHABLE => "The station is not connected.",
        code::DESKTOP_CONTROL_OFF => "Desktop control is off on this deck.",
        code::RECORDING_NOT_ALLOWED => "This grant does not allow monitor recordings.",
        code::RECORDING_CAP_REACHED => "This grant has used its recording limit.",
        code::RECORDING_SECONDS_REACHED => "This grant has used its recording time limit.",
        code::INPUT_NOT_ALLOWED => "This grant does not allow desktop input.",
        code::INPUT_SCOPE_UNSUPPORTED => {
            "This build supports input only in this caller's preview windows."
        }
        code::INPUT_TOOL_MISSING => {
            "The keyboard input tool wtype is not installed on the target station."
        }
        code::INPUT_TOOL_UNSUPPORTED => {
            "The target desktop cannot use the configured keyboard input tool."
        }
        code::INPUT_BOARD_FORBIDDEN => {
            "Fleet Deck board and wall windows cannot receive agent input."
        }
        code::INPUT_CAP_REACHED => "This grant has used its input action limit.",
        code::INPUT_CHARACTERS_REACHED => "This grant has used its typed-character limit.",
        code::INPUT_DESTINATION_UNAVAILABLE => {
            "The pointer destination is not available inside this preview."
        }
        code::INPUT_FOCUS_UNKNOWN => {
            "The preview's input focus could not be verified. Input was not started."
        }
        code::FOCUS_CHANGED => {
            "The preview no longer has focus. Input stopped; some input may already have been sent."
        }
        code::INPUT_INTERRUPTED => {
            "Input was interrupted. Some input may already have been sent. Do not retry automatically."
        }
        code::SIGNED_OUT_FOR_INPUT => {
            "You were signed out because an agent typed on the station where this board is open."
        }
        code::SIGN_IN_BLOCKED_FOR_INPUT => {
            "An agent is typing on this station. Sign in again in a minute."
        }
        code::INPUT_FAILED => {
            "The input tool failed. Some input may already have been sent. Do not retry automatically."
        }
        code::INPUT_TIMEOUT => {
            "Input did not finish in time. Some input may already have been sent. Do not retry automatically."
        }
        _ => return None,
    })
}

/// R13 P7 §13: the fixed explanatory text of an unknown durable input outcome.
pub const INPUT_OUTCOME_UNKNOWN_TEXT: &str =
    "The input outcome is unknown. Inspect the preview before issuing another input request.";

/// The budgets, rates and size limits of §8.1, §8.2 and §4.9.
pub mod limits {
    /// §8.1: the default `desktop_cap` of a grant with `desktop` or `capture`.
    pub const DESKTOP_CAP_DEFAULT: u32 = 300;
    /// §8.1: the highest `desktop_cap`.
    pub const DESKTOP_CAP_MAX: u32 = 5000;
    /// §8.1: the default `capture_cap` of a grant with `capture`.
    pub const CAPTURE_CAP_DEFAULT: u32 = 20;
    /// §8.1: the highest `capture_cap`.
    pub const CAPTURE_CAP_MAX: u32 = 5000;
    /// §8.1: the default `image_byte_cap`, in bytes (64 MiB).
    pub const IMAGE_BYTE_CAP_DEFAULT: u64 = 67_108_864;
    /// §8.1: the highest `image_byte_cap`, in bytes (1 GiB).
    pub const IMAGE_BYTE_CAP_MAX: u64 = 1_073_741_824;
    /// §8.1: `image_byte_cap` is a multiple of this many bytes (1 MiB).
    pub const IMAGE_BYTE_CAP_UNIT: u64 = 1_048_576;
    /// §8.1: the bytes that one image fetch reserves and charges for good.
    pub const IMAGE_RESERVE_BYTES: u64 = 716_800;
    /// §4.9: the largest PNG that `read_capture_image` returns (700 KiB, one
    /// artifact chunk).
    pub const IMAGE_MAX_BYTES: u64 = 716_800;
    /// §8.1: the distinct well-formed mutation attempts of one grant.
    pub const ATTEMPTS_MAX: u32 = 10_000;
    /// §8.2: the rolling window of every rate, in milliseconds.
    pub const RATE_WINDOW_MS: u64 = 60_000;
    /// §8.2: new mutations per grant in one window.
    pub const MUTATIONS_PER_WINDOW: u32 = 30;
    /// §8.2: `notify` calls per caller session in one window.
    pub const NOTIFY_PER_WINDOW: u32 = 6;
    /// §8.2: new screenshots per grant in one window.
    pub const SCREENSHOTS_PER_WINDOW: u32 = 2;
    /// §8.2: metadata and status queries per grant in one window.
    pub const QUERIES_PER_WINDOW: u32 = 60;
    /// §8.2: image fetches per grant in one window.
    pub const IMAGES_PER_WINDOW: u32 = 6;
    /// §8.2: mutations in flight per grant.
    pub const MUTATIONS_IN_FLIGHT: u32 = 2;
    /// §8.2: image fetches in flight per grant.
    pub const IMAGES_IN_FLIGHT: u32 = 1;
    /// §8.2: P6 calls and read permits in flight on the deck.
    pub const DECK_CALLS_IN_FLIGHT: u32 = 64;
    /// §8.2: image assemblies in flight on the deck.
    pub const DECK_ASSEMBLIES_IN_FLIGHT: u32 = 4;
    /// §4.1: the largest serialized `desktop_info` result, in bytes (16 KiB).
    pub const INFO_MAX_BYTES: u64 = 16_384;
    /// §4.1: the most monitors in a `desktop_info` result.
    pub const INFO_MAX_MONITORS: u32 = 64;
    /// §4.1: the most wall entries in a `desktop_info` result.
    pub const INFO_MAX_WALLS: u32 = 64;
    /// §4.9: the largest serialized MCP tool result, in bytes (1 MiB; the
    /// control frame limit is the same number).
    pub const MCP_RESULT_MAX_BYTES: u64 = 1_048_576;
    /// R13 P5b-2 §13.2: the default `recording_cap` of a recording grant.
    pub const RECORDING_CAP_DEFAULT: u32 = 5;
    /// R13 P5b-2 §13.2: the highest `recording_cap`.
    pub const RECORDING_CAP_MAX: u32 = 5000;
    /// R13 P5b-2 §13.2: the default `recording_second_cap` of a recording
    /// grant.
    pub const RECORDING_SECOND_CAP_DEFAULT: u32 = 600;
    /// R13 P5b-2 §13.2: the highest `recording_second_cap`.
    pub const RECORDING_SECOND_CAP_MAX: u32 = 600_000;
    /// R13 P7 §9.1: the default `input_cap` of a grant with `allow_input`.
    pub const INPUT_CAP_DEFAULT: u32 = 100;
    /// R13 P7 §9.1: the highest `input_cap`.
    pub const INPUT_CAP_MAX: u32 = 5000;
    /// R13 P7 §9.1: the default `input_character_cap` of a grant with
    /// `allow_input`.
    pub const INPUT_CHARACTER_CAP_DEFAULT: u32 = 20_000;
    /// R13 P7 §9.1: the highest `input_character_cap`.
    pub const INPUT_CHARACTER_CAP_MAX: u32 = 1_000_000;
    /// R13 P7 §9.3: new input mutations per grant in one window.
    pub const INPUT_PER_GRANT_WINDOW: u32 = 20;
    /// R13 P7 §9.3: new input mutations per caller session, across grants,
    /// in one window.
    pub const INPUT_PER_SESSION_WINDOW: u32 = 20;
    /// R13 P7 §9.3: active input operations per target station.
    pub const INPUT_PER_STATION_IN_FLIGHT: u32 = 1;
}

/// The total deadlines of §9, in milliseconds, from entry to the named
/// stage. A row that §9 marks "Existing" has no constant here: it keeps the
/// existing constant.
pub mod deadline_ms {
    /// §9 row "Caller-policy request".
    pub const CALLER_POLICY: u64 = 3_000;
    /// §9 row "Target-policy request".
    pub const TARGET_POLICY: u64 = 3_000;
    /// §9 row "Target → deck authority check, including caller policy".
    pub const AUTHORITY_CHECK: u64 = 5_000;
    /// §9 row "Caller MCP screenshot response".
    pub const SCREENSHOT_MCP: u64 = 45_000;
    /// §9 row "Deck preview wait".
    pub const PREVIEW_DECK_WAIT: u64 = 95_000;
    /// §9 row "Deck preview handler".
    pub const PREVIEW_DECK_HANDLER: u64 = 100_000;
    /// §9 row "Caller MCP preview response".
    pub const PREVIEW_MCP: u64 = 105_000;
    /// §9 row "Deck wait for those actions" (other station desktop actions).
    pub const ACTION_DECK_WAIT: u64 = 35_000;
    /// §9 row "Deck handler" (other station desktop actions).
    pub const ACTION_DECK_HANDLER: u64 = 40_000;
    /// §9 row "Caller MCP response" (other station desktop actions).
    pub const ACTION_MCP: u64 = 45_000;
    /// §9 row "Fresh desktop information", the station part.
    pub const INFO_STATION: u64 = 30_000;
    /// §9 row "Fresh desktop information", the deck part.
    pub const INFO_DECK: u64 = 40_000;
    /// §9 row "Fresh desktop information", the MCP part.
    pub const INFO_MCP: u64 = 45_000;
    /// §9 row "Capture result lookup", the station part.
    pub const CAPTURE_LOOKUP_STATION: u64 = 5_000;
    /// §9 row "Capture result lookup", the deck part.
    pub const CAPTURE_LOOKUP_DECK: u64 = 10_000;
    /// §9 row "Capture result lookup", the MCP part.
    pub const CAPTURE_LOOKUP_MCP: u64 = 15_000;
    /// §9 row "Other action status", the deck part.
    pub const STATUS_DECK: u64 = 10_000;
    /// §9 row "Other action status", the MCP part.
    pub const STATUS_MCP: u64 = 15_000;
    /// §9 row "One artifact-file request".
    pub const FILE_REQUEST: u64 = 20_000;
    /// §9 row "Complete image-file acquisition".
    pub const IMAGE_ACQUISITION: u64 = 60_000;
    /// §9 row "Deck image query, including final authorization".
    pub const IMAGE_DECK: u64 = 65_000;
    /// §9 row "Caller MCP image response, including encoding".
    pub const IMAGE_MCP: u64 = 70_000;
    /// R13 P5b-2 §13.5: the caller MCP response deadline of `record_monitor`
    /// is `seconds * 1000 + RECORD_MCP_EXTRA`.
    pub const RECORD_MCP_EXTRA: u64 = 45_000;
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODES: [&str; 54] = [
        code::NOT_BOUND,
        code::NOT_GRANTED,
        code::BINDING_STALE,
        code::ELEVATION_REQUIRED,
        code::UNSUPPORTED,
        code::INCARNATION_MISMATCH,
        code::EXPIRED,
        code::INVALID_PARAMS,
        code::REQUEST_ID_CONFLICT,
        code::DESKTOP_CAP_REACHED,
        code::CAPTURE_CAP_REACHED,
        code::IMAGE_BUDGET_REACHED,
        code::REQUEST_BUDGET_REACHED,
        code::RATE_LIMITED,
        code::BUSY,
        code::AUTHORITY_LOST,
        code::AUTHORITY_UNKNOWN,
        code::FLEET_CONTROL_OFF,
        code::DESKTOP_RESULT_TOO_LARGE,
        code::IMAGE_TOO_LARGE,
        code::IMAGE_INVALID,
        code::UNKNOWN_ARTIFACT,
        code::DESKTOP_WINDOW_NOT_FOUND,
        code::DESKTOP_DISPLAY_NOT_FOUND,
        code::DESKTOP_REFERENCE_NOT_GRANTED,
        code::DESKTOP_READ_TIMEOUT,
        code::DESKTOP_UNSUPPORTED,
        code::DESKTOP_FORBIDDEN,
        code::INVALID_DESKTOP_ACTION,
        code::DESKTOP_NOT_FOUND,
        code::DESKTOP_CONFLICT,
        code::DESKTOP_FAILED,
        code::DESKTOP_OVERLOADED,
        code::DESKTOP_TIMEOUT,
        code::STATION_UNREACHABLE,
        code::DESKTOP_CONTROL_OFF,
        code::RECORDING_NOT_ALLOWED,
        code::RECORDING_CAP_REACHED,
        code::RECORDING_SECONDS_REACHED,
        code::INPUT_NOT_ALLOWED,
        code::INPUT_SCOPE_UNSUPPORTED,
        code::INPUT_TOOL_MISSING,
        code::INPUT_TOOL_UNSUPPORTED,
        code::INPUT_BOARD_FORBIDDEN,
        code::INPUT_CAP_REACHED,
        code::INPUT_CHARACTERS_REACHED,
        code::INPUT_DESTINATION_UNAVAILABLE,
        code::INPUT_FOCUS_UNKNOWN,
        code::FOCUS_CHANGED,
        code::INPUT_INTERRUPTED,
        code::SIGNED_OUT_FOR_INPUT,
        code::SIGN_IN_BLOCKED_FOR_INPUT,
        code::INPUT_FAILED,
        code::INPUT_TIMEOUT,
    ];

    #[test]
    fn every_code_has_a_text_and_an_unknown_code_has_none() {
        for c in CODES {
            let t = text(c).unwrap_or_else(|| panic!("no text for {c}"));
            assert!(t.ends_with('.'), "{c}: {t}");
        }
        let mut sorted = CODES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), CODES.len(), "a code twice");
        assert_eq!(text("nope"), None);
        assert_eq!(text(""), None);
    }

    #[test]
    fn the_texts_are_the_exact_words_of_the_contract() {
        for (c, t) in [
            (
                code::NOT_BOUND,
                "This coordinator is not bound to a provider session.",
            ),
            (
                code::NOT_GRANTED,
                "This grant does not allow this desktop operation.",
            ),
            (
                code::BINDING_STALE,
                "The coordinator binding has changed. Start a new authorized request.",
            ),
            (
                code::ELEVATION_REQUIRED,
                "Owner elevation is required for desktop or capture authority.",
            ),
            (
                code::UNSUPPORTED,
                "The selected peers do not support this desktop grant.",
            ),
            (
                code::INCARNATION_MISMATCH,
                "The deck identity has changed. Reconnect before trying again.",
            ),
            (
                code::EXPIRED,
                "The desktop request expired before it was accepted.",
            ),
            (
                code::INVALID_PARAMS,
                "The desktop tool arguments are not valid.",
            ),
            (
                code::REQUEST_ID_CONFLICT,
                "This request ID was used for another desktop request.",
            ),
            (
                code::DESKTOP_CAP_REACHED,
                "This grant has used its desktop action limit.",
            ),
            (
                code::CAPTURE_CAP_REACHED,
                "This grant has used its screenshot limit.",
            ),
            (
                code::IMAGE_BUDGET_REACHED,
                "This grant has used its screenshot image-read budget.",
            ),
            (
                code::REQUEST_BUDGET_REACHED,
                "This grant has used its desktop request limit.",
            ),
            (
                code::RATE_LIMITED,
                "This grant's desktop rate limit has been reached. Try again later.",
            ),
            (
                code::BUSY,
                "The desktop operation cannot start because its capacity limit is in use.",
            ),
            (
                code::AUTHORITY_LOST,
                "Desktop authority was withdrawn before this operation could continue.",
            ),
            (
                code::AUTHORITY_UNKNOWN,
                "Desktop authority could not be checked. No further desktop work was started.",
            ),
            (
                code::FLEET_CONTROL_OFF,
                "Fleet control is off on this deck.",
            ),
            (
                code::DESKTOP_RESULT_TOO_LARGE,
                "The desktop information exceeds the tool response limit.",
            ),
            (
                code::IMAGE_TOO_LARGE,
                "The screenshot exceeds the 700 KiB MCP image limit. Open the artifact on the board.",
            ),
            (
                code::IMAGE_INVALID,
                "The screenshot artifact could not be verified.",
            ),
            (
                code::UNKNOWN_ARTIFACT,
                "No such capture artifact is available to this grant.",
            ),
            (
                code::DESKTOP_WINDOW_NOT_FOUND,
                "No window opened by this grant matches that handle.",
            ),
            (
                code::DESKTOP_DISPLAY_NOT_FOUND,
                "No wall available to this grant matches that display.",
            ),
            (
                code::DESKTOP_REFERENCE_NOT_GRANTED,
                "This grant cannot show that reference on the wall.",
            ),
            (
                code::DESKTOP_READ_TIMEOUT,
                "The desktop data did not finish loading in time.",
            ),
            (
                code::DESKTOP_UNSUPPORTED,
                "This station's build has no desktop control.",
            ),
            (
                code::DESKTOP_FORBIDDEN,
                "The station refused the desktop action.",
            ),
            (
                code::INVALID_DESKTOP_ACTION,
                "The desktop action is not valid.",
            ),
            (
                code::DESKTOP_NOT_FOUND,
                "The target of the desktop action was not found.",
            ),
            (
                code::DESKTOP_CONFLICT,
                "The desktop action conflicts with the current state.",
            ),
            (code::DESKTOP_FAILED, "The desktop action failed."),
            (
                code::DESKTOP_OVERLOADED,
                "The station is busy; try again later.",
            ),
            (
                code::DESKTOP_TIMEOUT,
                "The station did not answer in time; the action may have run.",
            ),
            (code::STATION_UNREACHABLE, "The station is not connected."),
            (
                code::DESKTOP_CONTROL_OFF,
                "Desktop control is off on this deck.",
            ),
            (
                code::RECORDING_NOT_ALLOWED,
                "This grant does not allow monitor recordings.",
            ),
            (
                code::RECORDING_CAP_REACHED,
                "This grant has used its recording limit.",
            ),
            (
                code::RECORDING_SECONDS_REACHED,
                "This grant has used its recording time limit.",
            ),
            (
                code::INPUT_NOT_ALLOWED,
                "This grant does not allow desktop input.",
            ),
            (
                code::INPUT_SCOPE_UNSUPPORTED,
                "This build supports input only in this caller's preview windows.",
            ),
            (
                code::INPUT_TOOL_MISSING,
                "The keyboard input tool wtype is not installed on the target station.",
            ),
            (
                code::INPUT_TOOL_UNSUPPORTED,
                "The target desktop cannot use the configured keyboard input tool.",
            ),
            (
                code::INPUT_BOARD_FORBIDDEN,
                "Fleet Deck board and wall windows cannot receive agent input.",
            ),
            (
                code::INPUT_CAP_REACHED,
                "This grant has used its input action limit.",
            ),
            (
                code::INPUT_CHARACTERS_REACHED,
                "This grant has used its typed-character limit.",
            ),
            (
                code::INPUT_DESTINATION_UNAVAILABLE,
                "The pointer destination is not available inside this preview.",
            ),
            (
                code::INPUT_FOCUS_UNKNOWN,
                "The preview's input focus could not be verified. Input was not started.",
            ),
            (
                code::FOCUS_CHANGED,
                "The preview no longer has focus. Input stopped; some input may already have been sent.",
            ),
            (
                code::INPUT_INTERRUPTED,
                "Input was interrupted. Some input may already have been sent. Do not retry automatically.",
            ),
            (
                code::SIGNED_OUT_FOR_INPUT,
                "You were signed out because an agent typed on the station where this board is open.",
            ),
            (
                code::SIGN_IN_BLOCKED_FOR_INPUT,
                "An agent is typing on this station. Sign in again in a minute.",
            ),
            (
                code::INPUT_FAILED,
                "The input tool failed. Some input may already have been sent. Do not retry automatically.",
            ),
            (
                code::INPUT_TIMEOUT,
                "Input did not finish in time. Some input may already have been sent. Do not retry automatically.",
            ),
        ] {
            assert_eq!(text(c), Some(t), "{c}");
        }
    }

    #[test]
    fn the_limits_and_deadlines_have_the_numbers_of_the_contract() {
        assert_eq!(limits::IMAGE_MAX_BYTES, crate::artifacts::MAX_CHUNK_BYTES);
        assert_eq!(limits::IMAGE_RESERVE_BYTES, limits::IMAGE_MAX_BYTES);
        assert_eq!(
            limits::IMAGE_BYTE_CAP_DEFAULT % limits::IMAGE_BYTE_CAP_UNIT,
            0
        );
        assert_eq!(limits::IMAGE_BYTE_CAP_MAX % limits::IMAGE_BYTE_CAP_UNIT, 0);
        assert_eq!(
            limits::MCP_RESULT_MAX_BYTES,
            crate::link::MAX_CONTROL_FRAME_BYTES as u64
        );
        // §4.9: the base64 of the largest image fits one result.
        let base64 = limits::IMAGE_MAX_BYTES.div_ceil(3) * 4;
        assert_eq!(base64, 955_736);
        assert!(base64 < limits::MCP_RESULT_MAX_BYTES);
        // §13.2: the recording defaults are inside their ranges.
        assert_eq!(limits::RECORDING_CAP_DEFAULT, 5);
        assert_eq!(limits::RECORDING_CAP_MAX, 5000);
        assert_eq!(limits::RECORDING_SECOND_CAP_DEFAULT, 600);
        assert_eq!(limits::RECORDING_SECOND_CAP_MAX, 600_000);
        assert_eq!(deadline_ms::RECORD_MCP_EXTRA, 45_000);
        // R13 P7 §9: the input defaults are inside their ranges.
        assert_eq!(limits::INPUT_CAP_DEFAULT, 100);
        assert_eq!(limits::INPUT_CAP_MAX, 5000);
        assert_eq!(limits::INPUT_CHARACTER_CAP_DEFAULT, 20_000);
        assert_eq!(limits::INPUT_CHARACTER_CAP_MAX, 1_000_000);
        assert_eq!(limits::INPUT_PER_GRANT_WINDOW, 20);
        assert_eq!(limits::INPUT_PER_SESSION_WINDOW, 20);
        assert_eq!(limits::INPUT_PER_STATION_IN_FLIGHT, 1);
        const {
            assert!(limits::INPUT_CAP_DEFAULT <= limits::INPUT_CAP_MAX);
            assert!(limits::INPUT_CHARACTER_CAP_DEFAULT <= limits::INPUT_CHARACTER_CAP_MAX);
            assert!(limits::INPUT_PER_GRANT_WINDOW <= limits::MUTATIONS_PER_WINDOW);
        }
        assert_eq!(
            INPUT_OUTCOME_UNKNOWN_TEXT,
            "The input outcome is unknown. Inspect the preview before issuing another input request."
        );
        // §9: every MCP deadline is above its deck deadline.
        const {
            assert!(deadline_ms::AUTHORITY_CHECK >= deadline_ms::CALLER_POLICY);
            assert!(deadline_ms::PREVIEW_DECK_WAIT < deadline_ms::PREVIEW_DECK_HANDLER);
            assert!(deadline_ms::PREVIEW_DECK_HANDLER < deadline_ms::PREVIEW_MCP);
            assert!(deadline_ms::ACTION_DECK_WAIT < deadline_ms::ACTION_DECK_HANDLER);
            assert!(deadline_ms::ACTION_DECK_HANDLER < deadline_ms::ACTION_MCP);
            assert!(deadline_ms::IMAGE_ACQUISITION < deadline_ms::IMAGE_DECK);
            assert!(deadline_ms::IMAGE_DECK < deadline_ms::IMAGE_MCP);
        }
    }
}
