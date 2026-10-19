//! Tests of the P6 to P7b coordinator desktop codes, texts and limits.

use super::*;

const CODES: [&str; 60] = [
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
    code::INPUT_CLICK_NOT_ALLOWED,
    code::INPUT_SCOPE_NOT_ALLOWED,
    code::INPUT_CLICK_DISABLED,
    code::INPUT_ANY_DISABLED,
    code::INPUT_CLICK_TOOL_MISSING,
    code::INPUT_CLICK_TOOL_UNSUPPORTED,
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
            "You were signed out because an agent used desktop input on the station where this board is open.",
        ),
        (
            code::SIGN_IN_BLOCKED_FOR_INPUT,
            "An agent is using desktop input on this station. Sign in again in a minute.",
        ),
        (
            code::INPUT_FAILED,
            "The input tool failed. Some input may already have been sent. Do not retry automatically.",
        ),
        (
            code::INPUT_TIMEOUT,
            "Input did not finish in time. Some input may already have been sent. Do not retry automatically.",
        ),
        (
            code::INPUT_CLICK_NOT_ALLOWED,
            "This grant does not allow pointer clicks.",
        ),
        (
            code::INPUT_SCOPE_NOT_ALLOWED,
            "This grant does not allow input in other application windows.",
        ),
        (
            code::INPUT_CLICK_DISABLED,
            "Agent pointer clicks are disabled on the target station.",
        ),
        (
            code::INPUT_ANY_DISABLED,
            "Agent input in other application windows is disabled on the target station.",
        ),
        (
            code::INPUT_CLICK_TOOL_MISSING,
            "The pointer click tool wlrctl is not installed on the target station.",
        ),
        (
            code::INPUT_CLICK_TOOL_UNSUPPORTED,
            "The target desktop cannot use the configured pointer click tool.",
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

#[test]
fn text_any_has_the_four_broader_texts_and_falls_back_to_text() {
    for (c, t) in [
        (
            code::DESKTOP_WINDOW_NOT_FOUND,
            "No input window available to this grant matches that handle.",
        ),
        (
            code::INPUT_DESTINATION_UNAVAILABLE,
            "The pointer destination is not available inside the target window.",
        ),
        (
            code::INPUT_FOCUS_UNKNOWN,
            "The target window's input focus could not be verified. No further input was started; the pointer may already have moved.",
        ),
        (
            code::FOCUS_CHANGED,
            "The target window no longer has focus. Input stopped; some input may already have been sent.",
        ),
    ] {
        assert_eq!(text_any(c), Some(t), "{c}");
        assert_ne!(text(c), Some(t), "{c}: the legacy text stays");
    }
    // Every other code, and an unknown one, answers as `text` does.
    for c in CODES {
        if ![
            code::DESKTOP_WINDOW_NOT_FOUND,
            code::INPUT_DESTINATION_UNAVAILABLE,
            code::INPUT_FOCUS_UNKNOWN,
            code::FOCUS_CHANGED,
        ]
        .contains(&c)
        {
            assert_eq!(text_any(c), text(c), "{c}");
        }
    }
    assert_eq!(text_any("nope"), None);
}

#[test]
fn the_p7b_limits_deadlines_and_outcome_text_have_the_numbers_of_the_contract() {
    assert_eq!(limits::WINDOW_LIST_PER_WINDOW, 10);
    assert_eq!(limits::WINDOW_LIST_IN_FLIGHT_PER_STATION, 1);
    assert_eq!(limits::WINDOW_LIST_MAX_WINDOWS, 64);
    assert_eq!(limits::WINDOW_LIST_MAX_MONITORS, 64);
    assert_eq!(limits::WINDOW_LIST_MAX_BYTES, 16_384);
    assert_eq!(limits::LISTED_HANDLE_TTL_MS, 60_000);
    assert_eq!(limits::LISTED_HANDLES_PER_BINDING, 64);
    assert_eq!(limits::LISTED_HANDLES_PER_STATION, 1024);
    assert_eq!(deadline_ms::CLICK_FINAL_BUNDLE, 250);
    assert_eq!(deadline_ms::CLICK_LIMIT, 1_000);
    assert_eq!(deadline_ms::CLICK_OBSERVATION_MAX_AGE, 100);
    assert_eq!(
        INPUT_ANY_OUTCOME_UNKNOWN_TEXT,
        "The input outcome is unknown. Inspect the target application before issuing another input request. Do not retry automatically."
    );
    // The P7 text is unchanged.
    assert_eq!(
        INPUT_OUTCOME_UNKNOWN_TEXT,
        "The input outcome is unknown. Inspect the preview before issuing another input request."
    );
}
