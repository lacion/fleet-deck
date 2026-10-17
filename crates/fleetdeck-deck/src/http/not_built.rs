//! The defined refusal of a part that is not built yet (R13 P2b/P3b step 1).
//!
//! Every stub route and every display arm of a handler that a step 2 task
//! builds answers `501 not_built`. The step 2 task replaces the stub; the
//! text stays fixed until then. A display never gets an owner payload from
//! a stub.

use axum::http::StatusCode;

use super::guard::ApiError;

/// The code of the refusal.
pub const CODE: &str = "not_built";

/// The refusal: `501 not_built`.
#[must_use]
pub fn not_built() -> ApiError {
    ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        CODE,
        "This part of the wall is not built yet.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_refusal_is_a_501_with_a_fixed_code() {
        let e = not_built();
        assert_eq!(e.status, StatusCode::NOT_IMPLEMENTED);
        assert_eq!(e.code, "not_built");
    }
}
