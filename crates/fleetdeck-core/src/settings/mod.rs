//! Settings of the deck and of a station (DL-172, design 1.1 and 1.3): the
//! field tables, the view, the save protocol and the secret store.

mod apply;
mod diff;
mod error;
mod fields;
mod fields_knobs;
mod fs;
mod holder;
pub mod refs;
mod store;
mod view;
mod writer;

pub use diff::{danger_keys, live_keys, restart_keys};
pub use error::SaveError;
pub use fields::{deck_fields, station_fields};
pub use fs::{Fs, RealFs};
pub use holder::{DeckHolder, Holder, StationHolder};
pub use view::{revision_of, view_deck, view_station};
pub use writer::SettingsWriter;

#[cfg(test)]
mod tests;
