//! The provider list of `station.info` (M2 P4-2) and the fresh probe of
//! `station fingerprint --probe-providers` (R13 P8 T2).

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::ProviderInstanceId;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::link::{StationProvider, VersionStatus};
use fleetdeck_providers::AuthState;
use fleetdeck_providers::ProviderAdapter;
use fleetdeck_providers::adapter::ProviderProbe;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use super::{PROBE_TIMEOUT, SessionManager};

/// The limits of a fresh provider probe (design `omarchy-p8.md` 5.3).
#[derive(Debug, Clone, Copy)]
struct ProbeLimits {
    /// The most time for one probe.
    each: Duration,
    /// The most time for the whole call.
    total: Duration,
    /// The most probes at the same time.
    concurrent: usize,
}

impl ProbeLimits {
    /// Ten seconds each, 30 seconds in all, four at the same time.
    const PRODUCTION: Self = Self {
        each: PROBE_TIMEOUT,
        total: Duration::from_secs(30),
        concurrent: 4,
    };
}

/// One probe of an adapter, bounded by `limit`. A probe that fails or runs
/// out of time gives no result.
async fn probe_adapter(adapter: &dyn ProviderAdapter, limit: Duration) -> Option<ProviderProbe> {
    tokio::time::timeout(limit, adapter.probe())
        .await
        .ok()
        .and_then(std::result::Result::ok)
}

impl SessionManager {
    /// The configured provider instances with the version and sign-in state
    /// of one probe each (`station.info`, M2 P4-2). The first call probes,
    /// each probe bounded by [`PROBE_TIMEOUT`]; later calls reuse the
    /// result until [`Self::refresh_providers`] replaces it (DL-059, B28).
    /// An instance without an adapter, or a failed probe, reports no
    /// version and `unknown` sign-in.
    pub async fn providers(&self) -> Vec<StationProvider> {
        let mut cache = self.providers.lock().await;
        if let Some(list) = cache.as_ref() {
            return self.with_live_modes(list.clone());
        }
        let list = self.probe_and_apply().await;
        *cache = Some(list.clone());
        self.with_live_modes(list)
    }

    /// The probe is cached; the default mode is a live setting (B27).
    fn with_live_modes(&self, mut list: Vec<StationProvider>) -> Vec<StationProvider> {
        let live = self.policy.current();
        for entry in &mut list {
            entry.default_mode = live
                .default_modes
                .reported(entry.provider)
                .map(str::to_owned);
        }
        list
    }

    /// Probes every instance now, replaces the cache of [`Self::providers`]
    /// and applies the result: a deny mark is set or cleared by the version
    /// status, and the handoff versions of each kind are replaced (B28
    /// §3.8). A quarantine is never touched. It runs after an attempt that
    /// changed or may have changed an install.
    pub async fn refresh_providers(&self) -> Vec<StationProvider> {
        let mut cache = self.providers.lock().await;
        let list = self.probe_and_apply().await;
        *cache = Some(list.clone());
        self.with_live_modes(list)
    }

    /// One probe of every configured instance, with the effects of its
    /// result. The caller holds the mutex of the cache.
    async fn probe_and_apply(&self) -> Vec<StationProvider> {
        let mut out = Vec::new();
        let policy = self.policy.current();
        for (instance, kind) in policy.instances() {
            let probe = match self.target.adapter(instance) {
                Some(adapter) => probe_adapter(adapter.as_ref(), PROBE_TIMEOUT).await,
                None => None,
            };
            let entry = self.provider_entry(instance, *kind, probe);
            // Starts of a denied instance are refused (R8.2, DL-162).
            self.target.set_denied(
                instance,
                *kind,
                entry.version_status == VersionStatus::Broken,
            );
            out.push(entry);
        }
        // The coordinator handoff check (DL-219).
        let mut kinds: Vec<ProviderKind> = Vec::new();
        for entry in &out {
            if !kinds.contains(&entry.provider) {
                kinds.push(entry.provider);
            }
        }
        for kind in kinds {
            let mut versions: Vec<String> = Vec::new();
            for entry in out.iter().filter(|e| e.provider == kind) {
                if let Some(v) = &entry.version
                    && !versions.contains(v)
                {
                    versions.push(v.clone());
                }
            }
            self.coordinator.replace_versions(kind, versions);
        }
        out
    }

    /// Like [`Self::providers`], but it probes again now: it does not read
    /// or fill the cache of [`Self::providers`], and it has no side effect
    /// (no `mark_broken`, no `record_version`). At most four probes run at
    /// the same time, each bounded by [`PROBE_TIMEOUT`], the whole call by
    /// 30 seconds; an instance without a result reports `unknown`. The
    /// order is the order of the configured instances.
    pub async fn providers_fresh(&self) -> Vec<StationProvider> {
        self.probe_fresh(ProbeLimits::PRODUCTION).await
    }

    async fn probe_fresh(&self, limits: ProbeLimits) -> Vec<StationProvider> {
        let policy = self.policy.current();
        let instances: Vec<(ProviderInstanceId, ProviderKind)> = policy
            .instances()
            .iter()
            .map(|(instance, kind)| (instance.clone(), *kind))
            .collect();
        let gate = Arc::new(Semaphore::new(limits.concurrent));
        let mut tasks = JoinSet::new();
        for (index, (instance, _)) in instances.iter().enumerate() {
            let Some(adapter) = self.target.adapter(instance) else {
                continue;
            };
            let gate = Arc::clone(&gate);
            tasks.spawn(async move {
                let probe = match gate.acquire().await {
                    Ok(_permit) => probe_adapter(adapter.as_ref(), limits.each).await,
                    Err(_) => None,
                };
                (index, probe)
            });
        }
        let mut probes: Vec<Option<ProviderProbe>> = instances.iter().map(|_| None).collect();
        let collect = async {
            while let Some(done) = tasks.join_next().await {
                match done {
                    Ok((index, probe)) => probes[index] = probe,
                    Err(e) => tracing::warn!(error = %e, "a fresh provider probe task failed"),
                }
            }
        };
        if tokio::time::timeout(limits.total, collect).await.is_err() {
            tracing::warn!("the fresh provider probe ran out of time; the rest report unknown");
        }
        // Stop the probes that did not finish and wait until they are gone.
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        instances
            .iter()
            .zip(probes)
            .map(|((instance, kind), probe)| self.provider_entry(instance, *kind, probe))
            .collect()
    }

    /// The report of one instance from its probe result: the one mapping of
    /// [`Self::providers`] and [`Self::providers_fresh`].
    fn provider_entry(
        &self,
        instance: &ProviderInstanceId,
        kind: ProviderKind,
        probe: Option<ProviderProbe>,
    ) -> StationProvider {
        let auth = match probe.as_ref().map(|p| &p.auth) {
            Some(AuthState::SignedIn { .. }) => "signed_in",
            Some(AuthState::SignedOut) => "signed_out",
            Some(AuthState::Unknown) | None => "unknown",
        };
        let version = probe.and_then(|p| p.version);
        let version_status = fleetdeck_providers::version::version_status(kind, version.as_deref());
        StationProvider {
            instance: instance.clone(),
            provider: kind,
            version,
            auth: auth.to_owned(),
            capabilities: self.target.adapter(instance).map(|a| a.capabilities()),
            version_status,
            default_mode: self
                .policy
                .current()
                .default_modes
                .reported(kind)
                .map(str::to_owned),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use fleetdeck_proto::Capabilities;
    use fleetdeck_proto::command::SessionSpec;
    use fleetdeck_providers::adapter::NativeSessionId;
    use fleetdeck_providers::{EventSink, LiveSession};
    use fleetdeck_testkit::Det;

    use super::*;
    use crate::manager::{Adapters, Policy};
    use crate::store::{OfflineApprovalPolicy, Store, StoreConfig};

    /// Counts probes and the most that ran at the same time.
    #[derive(Default)]
    struct Counts {
        calls: AtomicUsize,
        live: AtomicUsize,
        peak: AtomicUsize,
    }

    /// Lowers `live` when a probe ends or is cancelled.
    struct Running<'a>(&'a Counts);

    impl Drop for Running<'_> {
        fn drop(&mut self) {
            self.0.live.fetch_sub(1, Ordering::SeqCst);
        }
    }

    struct Fake {
        version: Option<&'static str>,
        delay: Duration,
        counts: Arc<Counts>,
    }

    #[async_trait]
    impl ProviderAdapter for Fake {
        fn kind(&self) -> ProviderKind {
            ProviderKind::Claude
        }
        fn capabilities(&self) -> Capabilities {
            fleetdeck_providers::claude::capabilities(false)
        }
        async fn probe(&self) -> fleetdeck_providers::adapter::Result<ProviderProbe> {
            self.counts.calls.fetch_add(1, Ordering::SeqCst);
            let live = self.counts.live.fetch_add(1, Ordering::SeqCst) + 1;
            self.counts.peak.fetch_max(live, Ordering::SeqCst);
            let _running = Running(&self.counts);
            tokio::time::sleep(self.delay).await;
            Ok(ProviderProbe {
                kind: ProviderKind::Claude,
                version: self.version.map(str::to_owned),
                auth: AuthState::SignedOut,
            })
        }
        async fn start(
            &self,
            _: SessionSpec,
            _: EventSink,
        ) -> fleetdeck_providers::adapter::Result<Box<dyn LiveSession>> {
            Err(fleetdeck_providers::ProviderError::SessionEnded)
        }
        async fn resume(
            &self,
            _: &NativeSessionId,
            _: SessionSpec,
            _: EventSink,
        ) -> fleetdeck_providers::adapter::Result<Box<dyn LiveSession>> {
            Err(fleetdeck_providers::ProviderError::SessionEnded)
        }
    }

    /// A manager with `n` instances `i0`, `i1`, ...; instance `k` sleeps
    /// `delays[k]` in its probe.
    async fn manager(delays: &[Duration], counts: &Arc<Counts>) -> SessionManager {
        let det = Det::new();
        let root = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        let mut instances = BTreeMap::new();
        let mut adapters = Adapters::new();
        for (k, delay) in delays.iter().enumerate() {
            let id: ProviderInstanceId = format!("i{k}").as_str().into();
            instances.insert(id.clone(), ProviderKind::Claude);
            let adapter: Arc<dyn ProviderAdapter> = Arc::new(Fake {
                version: Some(fleetdeck_providers::claude::TESTED_MIN_VERSION),
                delay: *delay,
                counts: Arc::clone(counts),
            });
            adapters.insert(id, adapter);
        }
        let policy = Policy::new(
            vec![root.path().to_owned()],
            4,
            OfflineApprovalPolicy::Wait,
            instances,
        );
        SessionManager::start(
            store,
            policy,
            adapters,
            det.ids(),
            Arc::new(crate::workspace::PrivatePaths::default()),
        )
        .await
        .unwrap()
    }

    const SHORT: Duration = Duration::from_millis(5);

    fn limits(each: Duration, total: Duration, concurrent: usize) -> ProbeLimits {
        ProbeLimits {
            each,
            total,
            concurrent,
        }
    }

    #[test]
    fn the_production_limits_are_ten_seconds_thirty_seconds_and_four() {
        let l = ProbeLimits::PRODUCTION;
        assert_eq!(l.each, Duration::from_secs(10));
        assert_eq!(l.total, Duration::from_secs(30));
        assert_eq!(l.concurrent, 4);
    }

    #[tokio::test]
    async fn a_fresh_probe_does_not_use_the_cache_of_station_info() {
        let counts = Arc::new(Counts::default());
        let manager = manager(&[SHORT], &counts).await;
        let fresh = manager.providers_fresh().await;
        let again = manager.providers_fresh().await;
        assert_eq!(
            counts.calls.load(Ordering::SeqCst),
            2,
            "two calls probe twice"
        );
        assert_eq!(fresh, again);
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].auth, "signed_out");
        assert_eq!(fresh[0].version_status, VersionStatus::Ok);
        // The cache of `providers()` is still empty: it probes now, once.
        let info = manager.providers().await;
        assert_eq!(counts.calls.load(Ordering::SeqCst), 3);
        assert_eq!(info, fresh);
        let _ = manager.providers().await;
        assert_eq!(counts.calls.load(Ordering::SeqCst), 3, "providers() caches");
        // A fresh probe after the cache still probes.
        let _ = manager.providers_fresh().await;
        assert_eq!(counts.calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn a_fresh_probe_has_no_side_effect() {
        let counts = Arc::new(Counts::default());
        let manager = manager(&[SHORT, SHORT], &counts).await;
        let fresh = manager.providers_fresh().await;
        assert!(fresh.iter().all(|p| p.version.is_some()));
        // `record_version` did not run. `mark_broken` cannot be seen from
        // here, and no real deny list has an entry; the method never calls it.
        assert!(
            manager
                .coordinator
                .versions(ProviderKind::Claude)
                .is_empty()
        );
        // `providers()` does record the version: the check above can fail.
        let _ = manager.providers().await;
        assert!(
            !manager
                .coordinator
                .versions(ProviderKind::Claude)
                .is_empty()
        );
    }

    #[tokio::test]
    async fn at_most_four_probes_run_at_the_same_time() {
        let counts = Arc::new(Counts::default());
        let manager = manager(&[Duration::from_millis(60); 7], &counts).await;
        let fresh = manager.providers_fresh().await;
        assert_eq!(fresh.len(), 7);
        assert!(fresh.iter().all(|p| p.version.is_some()));
        assert_eq!(counts.calls.load(Ordering::SeqCst), 7);
        let peak = counts.peak.load(Ordering::SeqCst);
        assert!((2..=4).contains(&peak), "peak {peak}");
    }

    #[tokio::test]
    async fn a_probe_past_its_limit_reports_unknown_and_the_order_is_kept() {
        let counts = Arc::new(Counts::default());
        // i0 answers, i1 hangs past the limit, i2 answers.
        let manager = manager(&[SHORT, Duration::from_secs(60), SHORT], &counts).await;
        let fresh = manager
            .probe_fresh(limits(
                Duration::from_millis(200),
                Duration::from_secs(30),
                4,
            ))
            .await;
        let names: Vec<_> = fresh.iter().map(|p| p.instance.as_str()).collect();
        assert_eq!(names, ["i0", "i1", "i2"]);
        assert!(fresh[0].version.is_some());
        assert_eq!(fresh[1].version, None);
        assert_eq!(fresh[1].auth, "unknown");
        assert_eq!(fresh[1].version_status, VersionStatus::Unknown);
        assert!(fresh[1].capabilities.is_some(), "capabilities stay");
        assert!(fresh[2].version.is_some());
        assert_eq!(counts.live.load(Ordering::SeqCst), 0, "no probe is left");
    }

    #[tokio::test]
    async fn the_whole_call_has_a_limit_and_the_rest_report_unknown() {
        let counts = Arc::new(Counts::default());
        // One slot: i0 answers, then i1 and i2 cannot finish before `total`.
        let manager = manager(
            &[SHORT, Duration::from_secs(60), Duration::from_secs(60)],
            &counts,
        )
        .await;
        let started = std::time::Instant::now();
        let fresh = manager
            .probe_fresh(limits(
                Duration::from_secs(60),
                Duration::from_millis(300),
                1,
            ))
            .await;
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(fresh[0].version.is_some());
        assert_eq!(fresh[1].version, None);
        assert_eq!(fresh[2].version, None);
        assert_eq!(fresh[2].version_status, VersionStatus::Unknown);
        assert_eq!(counts.live.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn no_instances_give_an_empty_list() {
        let counts = Arc::new(Counts::default());
        let manager = manager(&[], &counts).await;
        assert!(manager.providers_fresh().await.is_empty());
    }
}
