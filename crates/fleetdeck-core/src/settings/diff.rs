//! Which keys a change touches: the danger keys (design 1.5) and the keys
//! that apply at once or at the next start (design 1.4).

use fleetdeck_proto::settings::{Apply, FieldMeta};

use super::holder::Holder;

/// The keys of the change `old` to `new` that need a recent sign-in. A
/// secret value that changes also counts (the holders compare a digest).
#[must_use]
pub fn danger_keys<H: Holder>(old: &H::Parsed, new: &H::Parsed) -> Vec<String> {
    let order = H::fields();
    let mut keys = H::danger(old, new);
    keys.sort_by_key(|k| order.iter().position(|f| f.key == *k));
    keys
}

/// The keys of `fields` with this `apply` whose value differs.
fn changed<H: Holder>(
    old: &H::Parsed,
    new: &H::Parsed,
    fields: &[FieldMeta],
    apply: Apply,
) -> Vec<String> {
    let (old, new) = (H::compare_values(old), H::compare_values(new));
    fields
        .iter()
        .filter(|f| f.apply == apply && !f.read_only && old.get(&f.key) != new.get(&f.key))
        .map(|f| f.key.clone())
        .collect()
}

/// The changed keys that apply at the next start.
#[must_use]
pub fn restart_keys<H: Holder>(old: &H::Parsed, new: &H::Parsed) -> Vec<String> {
    changed::<H>(old, new, &H::fields(), Apply::Restart)
}

/// The changed keys that apply at once.
#[must_use]
pub fn live_keys<H: Holder>(old: &H::Parsed, new: &H::Parsed) -> Vec<String> {
    changed::<H>(old, new, &H::fields(), Apply::Live)
}
