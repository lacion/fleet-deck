//! The pure decision of the secret file sync (Fleet Sync FS-3, revision 2 R5
//! and pre-PR finding 3): which copy of one file wins, from which station it
//! comes and which stations get it. No I/O.

use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_git_secrets::{SecretFileState, SecretPresence};

/// The copy that wins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Winner {
    pub digest: String,
    pub source: StationId,
    /// Two homes changed the file; the newest copy won.
    pub conflict: bool,
}

/// The outcome of [`decide`] for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub winner: Option<Winner>,
    /// The stations that get the winning copy, in position order, with the
    /// digest that the write must find (`None`: the file must be missing).
    pub targets: Vec<(StationId, Option<String>)>,
    /// The stations whose copy was `changed` and lost, with that copy's
    /// digest (only when the rule of the changed copies chose the winner). A
    /// loser that the run could not write keeps this digest as the one that
    /// the relay saw, so that it does not look like a new edit on the next
    /// run.
    pub losers: Vec<(StationId, String)>,
}

/// One station as [`decide`] sees it: its id and position, the digest that
/// the relay last saw or wrote there (`seen`) and the scanned state.
pub type Scanned = (StationId, u32, Option<String>, SecretFileState);

/// The newest copy of `copies`: the highest `mtime_ms`; a tie goes to the
/// lowest position.
fn newest<'a>(copies: &[&'a Copy]) -> Option<&'a Copy> {
    copies
        .iter()
        .copied()
        .max_by_key(|c| (c.mtime_ms, std::cmp::Reverse(c.position)))
}

/// A present copy with a digest.
#[derive(Debug)]
struct Copy {
    station: StationId,
    position: u32,
    seen: Option<String>,
    digest: String,
    size: u64,
    mtime_ms: i64,
}

fn no_winner() -> Decision {
    Decision {
        winner: None,
        targets: Vec::new(),
        losers: Vec::new(),
    }
}

/// Picks the winning copy of one file (revision R5), without any I/O.
///
/// - Candidates are the present copies. An empty file is a candidate only
///   when no copy has content.
/// - One distinct digest wins.
/// - Else `changed` are the candidates that differ from the digest that the
///   relay last saw on that station (all of them at a first sync: no station
///   has a seen digest). One distinct digest in `changed` wins; several: the
///   newest wins and the file has a conflict. The `changed` copies that lose
///   are the `losers`.
/// - Else (nothing changed) a copy that has the `accepted` digest (the winner
///   of the last run that had one) wins: a failed write of that run does not
///   let the loser win on the retry.
/// - Else the newest of the `settled` copies (equal to their seen digest)
///   wins; with none, the newest copy.
///
/// A tie of `mtime_ms` goes to the lowest position. The source is the newest
/// copy that has the winning digest. The targets are every present copy with
/// another digest (also an empty file) and every missing copy.
#[must_use]
pub fn decide(accepted: Option<&str>, states: &[Scanned]) -> Decision {
    let copies: Vec<Copy> = states
        .iter()
        .filter(|(_, _, _, s)| s.state == SecretPresence::Present)
        .filter_map(|(station, position, seen, s)| {
            Some(Copy {
                station: station.clone(),
                position: *position,
                seen: seen.clone(),
                digest: s.digest.clone()?,
                size: s.size.unwrap_or(0),
                mtime_ms: s.mtime_ms.unwrap_or(0),
            })
        })
        .collect();
    let any_content = copies.iter().any(|c| c.size > 0);
    let candidates: Vec<&Copy> = copies
        .iter()
        .filter(|c| !any_content || c.size > 0)
        .collect();
    if candidates.is_empty() {
        return no_winner();
    }
    let mut distinct: Vec<&str> = candidates.iter().map(|c| c.digest.as_str()).collect();
    distinct.sort_unstable();
    distinct.dedup();
    let mut losers: Vec<(StationId, String)> = Vec::new();
    let (digest, conflict) = if let [only] = distinct.as_slice() {
        ((*only).to_owned(), false)
    } else {
        let first_sync = states.iter().all(|(_, _, seen, _)| seen.is_none());
        let changed: Vec<&Copy> = candidates
            .iter()
            .copied()
            .filter(|c| first_sync || c.seen.as_deref().is_some_and(|s| s != c.digest))
            .collect();
        if changed.is_empty() {
            let accepted_copies: Vec<&Copy> = candidates
                .iter()
                .copied()
                .filter(|c| accepted == Some(c.digest.as_str()))
                .collect();
            let settled: Vec<&Copy> = candidates
                .iter()
                .copied()
                .filter(|c| c.seen.as_deref() == Some(c.digest.as_str()))
                .collect();
            let pool = if !accepted_copies.is_empty() {
                &accepted_copies
            } else if settled.is_empty() {
                &candidates
            } else {
                &settled
            };
            (newest_digest(pool), false)
        } else {
            let mut kinds: Vec<&str> = changed.iter().map(|c| c.digest.as_str()).collect();
            kinds.sort_unstable();
            kinds.dedup();
            let digest = newest_digest(&changed);
            losers = changed
                .iter()
                .filter(|c| c.digest != digest)
                .map(|c| (c.station.clone(), c.digest.clone()))
                .collect();
            (digest, kinds.len() > 1)
        }
    };
    let same: Vec<&Copy> = candidates
        .iter()
        .copied()
        .filter(|c| c.digest == digest)
        .collect();
    let Some(source) = newest(&same) else {
        return no_winner();
    };
    let mut targets: Vec<(u32, StationId, Option<String>)> = states
        .iter()
        .filter_map(|(station, position, _, s)| match s.state {
            SecretPresence::Present => match &s.digest {
                Some(d) if *d != digest => Some((*position, station.clone(), Some(d.clone()))),
                _ => None,
            },
            SecretPresence::Missing => Some((*position, station.clone(), None)),
            _ => None,
        })
        .collect();
    targets.sort_by_key(|(position, _, _)| *position);
    Decision {
        winner: Some(Winner {
            digest,
            source: source.station.clone(),
            conflict,
        }),
        targets: targets.into_iter().map(|(_, s, e)| (s, e)).collect(),
        losers,
    }
}

/// The digest of the newest of `pool` (a tie: the lowest position).
fn newest_digest(pool: &[&Copy]) -> String {
    newest(pool).map(|c| c.digest.clone()).unwrap_or_default()
}
