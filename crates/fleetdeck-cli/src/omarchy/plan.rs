//! The facts of an install plan (`board-gaps.md` §3.2 and §3.3): the files
//! that the installer would not write because they differ (an unrecorded
//! conflict) or because the owner edited them (a recorded edit), with the
//! identity of each. `install --replace-conflicts --plan-stdin` computes the
//! facts again under the lock and compares them with the reviewed ones.

use std::path::Path;

use fleetdeck_proto::omarchy_repair::{
    AssetBase, InstallPlanFacts, PlanBlock, PlanClassification, PlanFile,
};

use super::assets::{Base, FILES};
use super::fsx::Home;
use super::install::{Plan, decide};
use super::record::{self, Record, rel, sha256_hex};

/// The hash of this build's assets: SHA-256 over the ID name of each asset,
/// a NUL byte and its text, in the order of the install. The menu has no ID
/// and is left out.
#[must_use]
pub fn build_hash() -> String {
    let mut bytes = Vec::new();
    for asset in FILES {
        if let Some(id) = asset.id {
            bytes.extend_from_slice(id.as_str().as_bytes());
            bytes.push(0);
            bytes.extend_from_slice(asset.text.as_bytes());
        }
    }
    sha256_hex(&bytes)
}

fn base_of(base: Base) -> AssetBase {
    match base {
        Base::Omarchy => AssetBase::Omarchy,
        Base::Fleetdeck => AssetBase::Fleetdeck,
    }
}

/// The identity fields of a target.
#[derive(Clone, Copy)]
struct Meta {
    dev: u64,
    ino: u64,
    mode: u32,
    size: u64,
}

/// The facts of this home. `record` is the loaded install record (or the
/// default one when there is none).
///
/// # Errors
/// The install record cannot be read.
pub fn facts(home: &Home, record: &Record) -> anyhow::Result<InstallPlanFacts> {
    let record_sha256 = home
        .read(Path::new(record::RECORD_PATH))?
        .map(|data| sha256_hex(&data.bytes));
    let mut files = Vec::new();
    for asset in FILES {
        let Some(id) = asset.id else { continue };
        let rel = rel(asset.base, asset.path);
        let entry = record.entry(asset.base, asset.path);
        let make = |classification, meta: Option<Meta>, sha: Option<String>, ok: bool| PlanFile {
            asset_id: id,
            path: home.abs(&rel).display().to_string(),
            classification,
            base: base_of(asset.base),
            rel: asset.path.to_owned(),
            dev: meta.map(|m| m.dev),
            ino: meta.map(|m| m.ino),
            mode: meta.map(|m| m.mode),
            size: meta.map(|m| m.size),
            sha256: sha,
            replaceable: ok,
            blocked: (!ok).then_some(PlanBlock::UnsafePath),
        };
        match home.read(&rel) {
            Ok(None) => {}
            Ok(Some(data)) => {
                let classification = match decide(Some(&data.bytes), entry, asset) {
                    Plan::Conflict => PlanClassification::UnrecordedConflict,
                    Plan::Keep => PlanClassification::RecordedEdit,
                    _ => continue,
                };
                let meta = Meta {
                    dev: data.dev,
                    ino: data.ino,
                    mode: data.full_mode,
                    size: data.size,
                };
                files.push(make(
                    classification,
                    Some(meta),
                    Some(sha256_hex(&data.bytes)),
                    true,
                ));
            }
            // A link, a hard link, a file that is not yours or not regular,
            // a file over 8 MiB, or a path above it that is unsafe: the
            // bytes are not compared (the record decides).
            Err(_) => {
                let classification = if entry.is_some() {
                    PlanClassification::RecordedEdit
                } else {
                    PlanClassification::UnrecordedConflict
                };
                let meta = home.lstat(&rel).ok().flatten().map(|m| Meta {
                    dev: m.dev,
                    ino: m.ino,
                    mode: m.mode,
                    size: m.size,
                });
                files.push(make(classification, meta, None, false));
            }
        }
    }
    Ok(InstallPlanFacts {
        build_hash: build_hash(),
        record_sha256,
        files,
    })
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
