//! "Use Tailscale automatically" at the start of the deck (tailnet design
//! section 14.4).
//!
//! When the deck setting `tailnet_auto` is on (the default, design section
//! 15.2), [`apply_at_start`] runs before any listener binds. It reads
//! `tailscale status` inside one absolute deadline of 3 s and, per channel (`listen` and `link_listen`), replaces only the
//! entries that the mode added earlier (the provenance lists) with the
//! current tailnet addresses. Manual entries stay as they are. The result is
//! saved in one save through the normal settings writer: revision checked,
//! no elevation (the owner gave it when turning the mode on).
//!
//! The whole Tailscale call (the program choice with its file checks, the
//! process start, the output read, the child cleanup) runs on a dedicated
//! thread (design section 15.3). The start waits for it until the deadline
//! and then goes on without it; a generation number makes a late answer
//! inert.
//!
//! Nothing is saved when Tailscale is not available, runs in userspace
//! networking, does not answer in time, or when non-local binding is on. The
//! reason is kept for the Tailnet card ([`TailnetStart`]).

use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fleetdeck_core::config::DeckConfig;
use fleetdeck_core::tailscale::{
    self, DEFAULT_BOARD_PORT, DEFAULT_LINK_PORT, Plan, Status, TailscaleCli, TailscaleError,
};
use fleetdeck_core::tailscale_wsl::HostFacts;
use fleetdeck_proto::settings::SettingsChange;
use serde_json::{Map, Value, json};
use tokio::sync::oneshot;

use crate::http::settings_routes::{DeckSettings, deck_view};
use crate::store::{AuditResult, DeckStore, NewAudit};

/// The one deadline of the start check.
pub const DEADLINE: Duration = Duration::from_secs(3);

/// The text of §14.2: Tailscale on Windows, WSL with NAT networking.
pub const WSL_NAT: &str = "Tailscale runs on Windows and WSL uses NAT networking, so the tailnet address is not on this machine. Set `networkingMode=mirrored` in `%UserProfile%\\.wslconfig`, run `wsl --shutdown`, and start Fleet Deck again; or install Tailscale inside WSL.";
/// The reason of a start check that did not finish in time.
pub const HOST_TIMEOUT: &str = "The host checks did not finish within 3 seconds.";
const TIMEOUT: &str = "Tailscale did not answer within 3 seconds.";
const USERSPACE: &str =
    "Tailscale runs in userspace networking: set the tailnet addresses by hand.";
const NONLOCAL: &str = "Non-local binding is on; set the tailnet addresses by hand.";
const NOT_LOCAL: &str = "the address is not on this machine";

/// What the automatic start did, for the Tailnet card. Set once at start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TailnetStart {
    /// `tailnet_auto` was on at start.
    pub enabled: bool,
    /// Why the mode did not apply (`None`: applied, or nothing to do).
    pub start_reason: Option<String>,
    /// The automatic addresses that were not used, with the reason.
    pub skipped: Vec<(SocketAddr, String)>,
}

/// The shared state of the card: written once by `serve`, read by the route.
pub type SharedStart = Arc<Mutex<TailnetStart>>;

/// The "Restart the deck to use it" flag of the Tailnet card: the mode is on,
/// Tailscale is available and not in userspace networking, and the usable
/// tailnet addresses differ from the addresses of the automatic entries
/// (`recorded`; only the IPs are compared, not the ports).
#[must_use]
pub fn changed<'a>(
    enabled: bool,
    userspace: bool,
    usable: &[IpAddr],
    recorded: impl IntoIterator<Item = &'a SocketAddr>,
) -> bool {
    if !enabled || userspace {
        return false;
    }
    let recorded: BTreeSet<IpAddr> = recorded.into_iter().map(SocketAddr::ip).collect();
    let usable: BTreeSet<IpAddr> = usable.iter().copied().collect();
    recorded != usable
}

/// Writes the audit row `tailnet.auto` (actor `deck`) with the keys that the
/// start changed, when it changed any. It never has a value.
pub async fn audit(store: &DeckStore, old: &DeckConfig, new: &DeckConfig) {
    let keys = changed_keys(old, new);
    if !keys.is_empty() {
        store
            .audit_or_log(
                NewAudit::new("deck", "tailnet.auto", AuditResult::Ok).detail(keys.join(",")),
            )
            .await;
    }
}

/// The reason that an automatic entry cannot be used, or `None` when it is
/// assigned to an interface of this machine.
#[must_use]
pub fn unusable(addr: SocketAddr) -> Option<String> {
    (!tailscale::is_usable(addr.ip())).then(|| NOT_LOCAL.to_owned())
}

/// The reason of an automatic entry whose bind failed.
#[must_use]
pub fn bind_failed(kind: std::io::ErrorKind) -> String {
    format!("the address cannot be bound: {kind:?}")
}

/// The keys that differ between `old` and `new` among the keys that the
/// automatic start writes, in the order of the field table.
#[must_use]
pub fn changed_keys(old: &DeckConfig, new: &DeckConfig) -> Vec<String> {
    let mut keys = Vec::new();
    let mut add = |changed: bool, key: &str| {
        if changed {
            keys.push(key.to_owned());
        }
    };
    add(old.listen != new.listen, "listen");
    add(old.link_listen != new.link_listen, "link_listen");
    add(
        old.tailnet_hostname != new.tailnet_hostname,
        "tailnet_hostname",
    );
    add(
        old.tailnet_auto_listen != new.tailnet_auto_listen,
        "tailnet_auto_listen",
    );
    add(
        old.tailnet_auto_link_listen != new.tailnet_auto_link_listen,
        "tailnet_auto_link_listen",
    );
    keys
}

/// One channel: the list without the entries of its provenance list, plus
/// each wanted entry that the list does not hold. Returns the new list and
/// the new provenance list (the entries that were added). The port is the
/// port of the first entry of the list as it was saved, else `default_port`.
#[must_use]
pub fn replace_channel(
    list: &[SocketAddr],
    provenance: &[SocketAddr],
    usable: &[IpAddr],
    default_port: u16,
) -> (Vec<SocketAddr>, Vec<SocketAddr>) {
    let port = list.first().map_or(default_port, SocketAddr::port);
    let mut out: Vec<SocketAddr> = list
        .iter()
        .copied()
        .filter(|a| !provenance.contains(a))
        .collect();
    let mut added = Vec::new();
    for ip in usable {
        let wanted = SocketAddr::new(*ip, port);
        if !out.contains(&wanted) {
            out.push(wanted);
            added.push(wanted);
        }
    }
    (out, added)
}

fn off(reason: impl Into<String>) -> TailnetStart {
    TailnetStart {
        enabled: true,
        start_reason: Some(reason.into()),
        skipped: Vec::new(),
    }
}

/// Runs the automatic start. Returns the config (updated when a save
/// happened) and what to tell the card. `overrides` are the settings that
/// the command line gave: a channel that the command line set is left
/// alone. `deadline` is the one absolute deadline of the whole check (the
/// program lookup, the status call and the save): after it nothing is
/// saved.
pub async fn apply_at_start(
    settings: &DeckSettings,
    config: DeckConfig,
    overrides: &[(String, String)],
    facts: &HostFacts,
    is_usable: &(dyn Fn(IpAddr) -> bool + Sync),
    deadline: Instant,
) -> (DeckConfig, TailnetStart) {
    apply_at_start_with(
        settings,
        config,
        overrides,
        facts,
        is_usable,
        deadline,
        TailscaleCli::locate,
    )
    .await
}

/// The generation of the one call of a start. The start moves it on when it
/// stops waiting, so that a late answer is inert.
const FIRST: u64 = 1;

/// The whole Tailscale call, on the thread of the call: the program choice
/// `locate`, then `tailscale status` under `deadline` (the runner kills the
/// child when the time is gone).
fn call(
    configured: Option<&Path>,
    deadline: Instant,
    locate: impl FnOnce(Option<&Path>) -> TailscaleCli,
) -> Result<Status, TailscaleError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| TailscaleError::Spawn(e.to_string()))?;
    runtime.block_on(async {
        let cli = locate(configured);
        if Instant::now() >= deadline {
            return Err(TailscaleError::Timeout);
        }
        cli.status_until(deadline).await
    })
}

/// Starts the dedicated thread of [`call`]. The answer comes on the channel
/// while `generation` is still [`FIRST`]; the thread is never joined.
fn spawn_call(
    configured: Option<PathBuf>,
    deadline: Instant,
    locate: impl FnOnce(Option<&Path>) -> TailscaleCli + Send + 'static,
    generation: Arc<AtomicU64>,
) -> std::io::Result<oneshot::Receiver<Result<Status, TailscaleError>>> {
    let (tx, rx) = oneshot::channel();
    std::thread::Builder::new()
        .name("tailnet-start".to_owned())
        .spawn(move || {
            let answer = call(configured.as_deref(), deadline, locate);
            if generation.load(Ordering::SeqCst) == FIRST {
                // The start may have gone on already: the answer is dropped.
                let _ = tx.send(answer);
            }
        })?;
    Ok(rx)
}

/// [`apply_at_start`] with the program choice `locate` (a test gives a slow
/// one). `locate` runs on the thread of the Tailscale call, inside
/// `deadline`.
pub async fn apply_at_start_with(
    settings: &DeckSettings,
    config: DeckConfig,
    overrides: &[(String, String)],
    facts: &HostFacts,
    is_usable: &(dyn Fn(IpAddr) -> bool + Sync),
    deadline: Instant,
    locate: impl FnOnce(Option<&Path>) -> TailscaleCli + Send + 'static,
) -> (DeckConfig, TailnetStart) {
    if !config.tailnet_auto {
        return (config, TailnetStart::default());
    }
    if facts.nonlocal_bind {
        return (config, off(NONLOCAL));
    }
    let generation = Arc::new(AtomicU64::new(FIRST));
    let rx = match spawn_call(
        config.tailscale_bin.clone(),
        deadline,
        locate,
        Arc::clone(&generation),
    ) {
        Ok(rx) => rx,
        Err(e) => {
            tracing::warn!(error = %e, "automatic tailnet start: thread");
            let reason = format!("Tailscale could not be started: {:?}.", e.kind());
            return (config, off(reason));
        }
    };
    let waited = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), rx).await;
    let status = match waited {
        Ok(Ok(Ok(status))) => status,
        Ok(Ok(Err(TailscaleError::Timeout))) | Ok(Err(_)) | Err(_) => {
            // Go on without the answer; the thread sends nothing now.
            generation.fetch_add(1, Ordering::SeqCst);
            return (config, off(TIMEOUT));
        }
        Ok(Ok(Err(e))) => {
            return (
                config,
                off(format!("Tailscale is not available: {}.", e.reason())),
            );
        }
    };
    let plan = tailscale::plan_use(&status, &[], &[], is_usable);
    if plan.userspace {
        let reason = if facts.wsl { WSL_NAT } else { USERSPACE };
        return (config, off(reason));
    }
    let overridden = |key: &str| overrides.iter().any(|(k, _)| k == key);
    let listen = (!overridden("listen")).then(|| {
        replace_channel(
            &config.listen,
            &config.tailnet_auto_listen,
            &plan.usable_ips,
            DEFAULT_BOARD_PORT,
        )
    });
    let link = (!overridden("link_listen")).then(|| {
        replace_channel(
            &config.link_listen,
            &config.tailnet_auto_link_listen,
            &plan.usable_ips,
            DEFAULT_LINK_PORT,
        )
    });
    let mut next = config.clone();
    if let Some((list, added)) = listen {
        next.listen = list;
        next.tailnet_auto_listen = added;
    }
    if let Some((list, added)) = link {
        next.link_listen = list;
        next.tailnet_auto_link_listen = added;
    }
    next.tailnet_hostname = hostname_of(&plan, &config);
    let keys = changed_keys(&config, &next);
    if keys.is_empty() {
        return (
            config,
            TailnetStart {
                enabled: true,
                ..TailnetStart::default()
            },
        );
    }
    // A late answer is inert: the generation and the deadline are checked
    // before the save.
    if generation.load(Ordering::SeqCst) != FIRST || Instant::now() >= deadline {
        generation.fetch_add(1, Ordering::SeqCst);
        return (config, off(TIMEOUT));
    }
    match save(settings, &next, &keys).await {
        Ok(()) => (
            next,
            TailnetStart {
                enabled: true,
                ..TailnetStart::default()
            },
        ),
        Err(code) => (
            config,
            off(format!("The settings could not be saved: {code}.")),
        ),
    }
}

fn hostname_of(plan: &Plan, config: &DeckConfig) -> Option<String> {
    plan.tailnet_hostname
        .clone()
        .or_else(|| config.tailnet_hostname.clone())
}

fn texts(list: &[SocketAddr]) -> Value {
    json!(list.iter().map(ToString::to_string).collect::<Vec<_>>())
}

/// Saves the `keys` of `next` in one save: the revision of the file is
/// checked, no elevation is needed. The error is the code of the save error.
async fn save(settings: &DeckSettings, next: &DeckConfig, keys: &[String]) -> Result<(), String> {
    let _gate = settings.gate.lock().await;
    let view = deck_view(settings).await.map_err(|e| {
        tracing::warn!(error = %e, "automatic tailnet start: deck settings");
        "settings_unreadable".to_owned()
    })?;
    let mut changes = Map::new();
    for key in keys {
        let value = match key.as_str() {
            "listen" => texts(&next.listen),
            "link_listen" => texts(&next.link_listen),
            "tailnet_hostname" => json!(next.tailnet_hostname),
            "tailnet_auto_listen" => texts(&next.tailnet_auto_listen),
            _ => texts(&next.tailnet_auto_link_listen),
        };
        changes.insert(key.clone(), value);
    }
    let change = SettingsChange {
        changes,
        revision: view.revision,
        elevation: None,
    };
    let writer = Arc::clone(&settings.writer);
    // The owner turned the mode on with an elevation: the new tailnet
    // addresses are not asked for again.
    let saved = tokio::task::spawn_blocking(move || {
        writer.save(&change, true, |_| Ok::<_, (String, String)>(()))
    })
    .await
    .map_err(|_| "settings_write_failed".to_owned())?;
    match saved {
        Ok(_) => Ok(()),
        Err(e) => {
            tracing::warn!(error = %e, "automatic tailnet start: save");
            Err(e.code().to_owned())
        }
    }
}

const LISTS: [(&str, &str); 2] = [
    ("listen", "tailnet_auto_listen"),
    ("link_listen", "tailnet_auto_link_listen"),
];

/// True when `changes` is a change that [`reconcile`] looks at.
#[must_use]
pub fn touches_provenance(changes: &Map<String, Value>) -> bool {
    changes.contains_key("tailnet_auto")
        || LISTS.iter().any(|(list, _)| changes.contains_key(*list))
}

fn addresses(value: &Value) -> Option<Vec<SocketAddr>> {
    value
        .as_array()?
        .iter()
        .map(|v| v.as_str()?.parse().ok())
        .collect()
}

/// The manual-save rule of design 14.4 and 15.2, for the settings routes:
/// `old` are the values before the save and `changes` the keys of the save.
/// A change that sets `tailnet_auto` to `false` (the resulting effective
/// value is off) sets both provenance lists to empty. `true` and a reset
/// (`null`: back to the default, which is on) clear nothing.
/// A change of `listen` (or `link_listen`) that does not set the matching
/// provenance list sets it to its old entries that are still in the new
/// list: an entry that the owner removed is forgotten, an entry that the
/// owner kept stays automatic. A value that does not parse is left for the
/// writer to refuse.
pub fn reconcile(old: &Map<String, Value>, changes: &mut Map<String, Value>) {
    if matches!(changes.get("tailnet_auto"), Some(Value::Bool(false))) {
        for (_, provenance) in LISTS {
            changes.insert(provenance.to_owned(), json!([]));
        }
        return;
    }
    for (list, provenance) in LISTS {
        if changes.contains_key(provenance) {
            continue;
        }
        let Some(new) = changes.get(list) else {
            continue;
        };
        // `null` removes the key: the default list has no tailnet entry.
        let new = if new.is_null() {
            Some(Vec::new())
        } else {
            addresses(new)
        };
        let (Some(new), Some(kept)) = (new, old.get(provenance).and_then(addresses)) else {
            continue;
        };
        let kept: Vec<SocketAddr> = kept.into_iter().filter(|a| new.contains(a)).collect();
        changes.insert(provenance.to_owned(), texts(&kept));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(text: &str) -> SocketAddr {
        text.parse().unwrap()
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn a_channel_replaces_only_its_provenance_entries() {
        let list = [
            a("127.0.0.1:7420"),
            a("192.168.5.5:7420"),
            a("100.64.0.7:7420"),
            a("100.64.0.9:7420"),
        ];
        // `.9` is automatic, `.7` is a manual entry in the tailnet range.
        let provenance = [a("100.64.0.9:7420")];
        let (out, added) = replace_channel(&list, &provenance, &[ip("100.64.0.10")], 7420);
        assert_eq!(
            out,
            vec![
                a("127.0.0.1:7420"),
                a("192.168.5.5:7420"),
                a("100.64.0.7:7420"),
                a("100.64.0.10:7420"),
            ]
        );
        assert_eq!(added, vec![a("100.64.0.10:7420")]);
    }

    #[test]
    fn a_wanted_address_that_is_already_manual_is_not_recorded() {
        let list = [a("127.0.0.1:7420"), a("100.64.0.10:7420")];
        let (out, added) = replace_channel(&list, &[], &[ip("100.64.0.10")], 7420);
        assert_eq!(out, list.to_vec());
        assert!(added.is_empty());
    }

    #[test]
    fn changed_compares_the_usable_addresses_with_the_recorded_ones() {
        let usable = [ip("100.64.0.10"), ip("fd7a:115c:a1e0::a")];
        let same = [a("100.64.0.10:7420"), a("[fd7a:115c:a1e0::a]:7421")];
        assert!(!changed(true, false, &usable, &same));
        // A new address, a missing address and an empty record differ.
        assert!(changed(true, false, &[ip("100.64.0.11")], &same));
        assert!(changed(true, false, &usable, &same[..1]));
        assert!(changed(true, false, &usable, &[]));
        // The mode off, or userspace networking: never.
        assert!(!changed(false, false, &usable, &[]));
        assert!(!changed(true, true, &usable, &[]));
    }

    #[test]
    fn the_port_is_the_port_of_the_first_entry_else_the_default() {
        let (out, _) = replace_channel(&[a("127.0.0.1:9000")], &[], &[ip("100.64.0.10")], 7420);
        assert_eq!(out.last(), Some(&a("100.64.0.10:9000")));
        let (out, _) = replace_channel(&[], &[], &[ip("100.64.0.10")], 7421);
        assert_eq!(out, vec![a("100.64.0.10:7421")]);
        let (out, added) = replace_channel(&[], &[], &[ip("fd7a:115c:a1e0::a")], 7421);
        assert_eq!(out, vec![a("[fd7a:115c:a1e0::a]:7421")]);
        assert_eq!(added, out);
    }
}
