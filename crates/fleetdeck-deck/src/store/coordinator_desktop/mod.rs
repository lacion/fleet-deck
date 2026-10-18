//! The deck store of the P6 coordinator desktop tools (R13 P6). The V34
//! migration is in [`schema`]; [`ops`] holds the operation ledger, [`admit`]
//! the one admission transaction (§6.2 step 9), [`budget`] the image
//! reservation, the query rate and the cap change, and [`rate`] the rolling
//! windows (§8.2) that the transactions use. No link and no HTTP handler is
//! here: the call layer uses these functions.

mod admit;
mod budget;
mod ops;
mod rate;
pub(super) mod schema;

pub use budget::{CapsChanged, CapsError};
pub use ops::{
    Admission, Charges, DesktopOp, NewOp, OpClass, OpOutcome, OpState, OpTool, request_key,
};

#[cfg(test)]
mod admit_tests;
#[cfg(test)]
mod budget_tests;
#[cfg(test)]
mod ops_tests;
#[cfg(test)]
mod rate_tests;
