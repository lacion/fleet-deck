#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use fleetdeck_proto::fleet_git::StationTraits;
use fleetdeck_proto::link::{StationInfo, StationProvider};
use fleetdeck_proto::resources::{CpuLoad, MemoryUse};
use fleetdeck_proto::{SessionSummary, StationId};

fn view(name: &str, live: u64) -> StationView {
    StationView {
        station_id: StationId::from(name.to_owned()),
        name: name.to_owned(),
        key: "k".to_owned(),
        direction: "deck_dials".to_owned(),
        dial_url: None,
        reachable: true,
        last_seen: None,
        cursor: 0,
        info: Some(StationInfo {
            name: name.to_owned(),
            incarnation: "inc".to_owned().into(),
            head: 0,
            live_sessions: live,
            version: None,
            max_sessions: None,
            allow_shell: None,
            allow_setup_cmd: None,
            providers: vec![StationProvider {
                instance: "claude".to_owned().into(),
                provider: ProviderKind::Claude,
                version: Some("2.1.0".to_owned()),
                auth: "signed_in".to_owned(),
                capabilities: None,
                version_status: Default::default(),
            }],
            assist_tokens_today: None,
            assist_cap: None,
            resources: None,
            desktop: None,
            traits: None,
            config: None,
            taildrive: None,
            fleet_git: None,
            fleet_summary: None,
        }),
        info_at: None,
        display_resources: None,
        coordinator_live: false,
    }
}

fn with_traits(
    mut st: StationView,
    os: &str,
    arch: &str,
    battery: Option<bool>,
    labels: &[&str],
) -> StationView {
    st.info.as_mut().unwrap().traits = Some(StationTraits {
        os: os.to_owned(),
        arch: arch.to_owned(),
        on_battery: battery,
        labels: labels.iter().map(|l| (*l).to_owned()).collect(),
    });
    st
}

fn with_room(
    mut st: StationView,
    cores: u32,
    load5_x100: u32,
    total: u64,
    available: u64,
) -> StationView {
    st.info.as_mut().unwrap().resources = Some(StationResources {
        at: 0,
        disks: Vec::new(),
        memory: Some(MemoryUse {
            total_bytes: total,
            available_bytes: available,
        }),
        cpu: Some(CpuLoad {
            cores,
            load1_x100: load5_x100,
            load5_x100,
        }),
        sessions: Vec::new(),
    });
    st
}

fn card(station: &str, status: CardStatus) -> DeckSession {
    DeckSession {
        station_id: StationId::from(station.to_owned()),
        summary: SessionSummary::new("s".into()),
        status,
        unread: None,
        project_id: None,
        review_rev: None,
    }
}

fn needs<'a>(requires: &'a [String], prefer: &'a [Prefer]) -> PlaceNeeds<'a> {
    PlaceNeeds {
        requires,
        prefer,
        provider: None,
        homes: None,
    }
}

fn names(ids: &[StationId]) -> Vec<&str> {
    ids.iter().map(|i| i.as_str()).collect()
}

fn req(entries: &[&str]) -> Vec<String> {
    entries.iter().map(|e| (*e).to_owned()).collect()
}

#[test]
fn a_requirement_removes_a_station_and_a_station_without_traits_meets_none() {
    let stations = vec![
        with_traits(view("a", 0), "linux", "x86_64", None, &["gpu"]),
        with_traits(view("b", 0), "linux", "x86_64", None, &[]),
        view("c", 0),
    ];
    let requires = req(&["gpu"]);
    let got = place(&stations, &[], &needs(&requires, &[]), 1, false).unwrap();
    assert_eq!(names(&got), ["a"]);
    // No requirement: the station without traits is a candidate.
    let all = rank(&stations, &[], &needs(&[], &[]));
    assert_eq!(all.len(), 3);
}

#[test]
fn os_and_arch_entries_match_the_traits() {
    let stations = vec![
        with_traits(view("a", 0), "linux", "x86_64", None, &[]),
        with_traits(view("b", 0), "macos", "aarch64", None, &[]),
    ];
    let requires = req(&["os:macos"]);
    assert_eq!(names(&rank(&stations, &[], &needs(&requires, &[]))), ["b"]);
    let requires = req(&["arch:x86_64"]);
    assert_eq!(names(&rank(&stations, &[], &needs(&requires, &[]))), ["a"]);
    let requires = req(&["os:linux", "arch:aarch64"]);
    assert_eq!(
        place(&stations, &[], &needs(&requires, &[]), 1, false),
        Err(PlaceError::NoneHasAll)
    );
}

#[test]
fn each_preference_orders_the_candidates() {
    let stations = vec![
        with_traits(view("a", 0), "linux", "x86_64", Some(true), &[]),
        with_traits(view("b", 0), "linux", "x86_64", Some(false), &[]),
        with_traits(view("c", 0), "linux", "x86_64", None, &["always_on"]),
    ];
    let pick = |prefer: &[Prefer], sessions: &[DeckSession]| {
        names(&rank(&stations, sessions, &needs(&[], prefer)))[0].to_owned()
    };
    assert_eq!(pick(&[Prefer::NotOnBattery], &[]), "b");
    assert_eq!(pick(&[Prefer::AlwaysOn], &[]), "c");
    // `idle`: only the stations without a working, verifying or queued card.
    let busy = [
        card("a", CardStatus::Working),
        card("b", CardStatus::Verifying),
    ];
    assert_eq!(pick(&[Prefer::Idle], &busy), "c");
    let done = [card("a", CardStatus::Ended), card("b", CardStatus::Queued)];
    assert_eq!(pick(&[Prefer::Idle], &done), "a");
    // The number of satisfied preferences comes first.
    assert_eq!(pick(&[Prefer::Idle, Prefer::AlwaysOn], &busy), "c");
    // An unknown preference is never satisfied.
    assert_eq!(pick(&[Prefer::Unknown], &[]), "a");
}

#[test]
fn the_free_resources_order_comes_after_the_preferences() {
    let stations = vec![
        with_room(view("a", 0), 4, 300, 100, 50),
        with_room(view("b", 0), 4, 100, 100, 80),
        view("c", 0),
        with_room(view("d", 0), 0, 0, 100, 90),
    ];
    // a: min(0.25, 0.5); b: min(0.75, 0.8); c: 0; d: cores 0 gives 0.
    let got = rank(&stations, &[], &needs(&[], &[]));
    assert_eq!(names(&got), ["b", "a", "c", "d"]);
}

#[test]
fn the_fewest_live_sessions_then_the_name_break_a_tie() {
    let stations = vec![
        view("zed", 1),
        view("beta", 2),
        view("alpha", 2),
        view("mid", 1),
    ];
    let got = rank(&stations, &[], &needs(&[], &[]));
    assert_eq!(names(&got), ["mid", "zed", "alpha", "beta"]);
}

#[test]
fn a_batch_counts_each_placement_as_a_live_session() {
    let stations = vec![view("a", 0), view("b", 2)];
    let got = place(&stations, &[], &needs(&[], &[]), 4, false).unwrap();
    // a: 0, then 1, 2 (ties by name go to a), then b has 2 and a has 3.
    assert_eq!(names(&got), ["a", "a", "a", "b"]);
}

#[test]
fn spread_uses_every_station_once_and_the_fourth_goes_to_the_best() {
    let stations = vec![
        with_traits(view("a", 0), "linux", "x86_64", None, &[]),
        with_traits(view("b", 0), "linux", "x86_64", None, &["always_on"]),
        view("c", 0),
    ];
    let prefer = [Prefer::AlwaysOn];
    let got = place(&stations, &[], &needs(&[], &prefer), 3, true).unwrap();
    let mut sorted = names(&got);
    sorted.sort_unstable();
    assert_eq!(sorted, ["a", "b", "c"]);
    assert_eq!(names(&got)[0], "b");
    let got = place(&stations, &[], &needs(&[], &prefer), 4, true).unwrap();
    assert_eq!(names(&got)[3], "b");
}

#[test]
fn homes_and_the_provider_narrow_the_candidates() {
    let stations = vec![view("a", 0), view("b", 0)];
    let homes = [StationId::from("b".to_owned())];
    let n = PlaceNeeds {
        homes: Some(&homes),
        ..needs(&[], &[])
    };
    assert_eq!(
        names(&place(&stations, &[], &n, 2, false).unwrap()),
        ["b", "b"]
    );
    let n = PlaceNeeds {
        homes: Some(&[]),
        ..needs(&[], &[])
    };
    assert_eq!(
        place(&stations, &[], &n, 1, false),
        Err(PlaceError::NoReadyHome)
    );
    let n = PlaceNeeds {
        provider: Some(ProviderKind::Codex),
        ..needs(&[], &[])
    };
    assert_eq!(
        place(&stations, &[], &n, 1, false),
        Err(PlaceError::NoneFree)
    );
}

#[test]
fn no_candidate_gives_the_error_texts() {
    let mut down = view("down", 0);
    down.reachable = false;
    let stations = vec![down];
    let requires = req(&["gpu"]);
    let err = place(&stations, &[], &needs(&requires, &[]), 1, false).unwrap_err();
    assert_eq!(err, PlaceError::NoneFree);
    assert_eq!(err.text(), "No station matches: no station is free.");
    assert!(rank(&stations, &[], &needs(&[], &[])).is_empty());

    let stations = vec![with_traits(
        view("a", 0),
        "linux",
        "x86_64",
        None,
        &["fast"],
    )];
    let requires = req(&["fast", "gpu", "big"]);
    let err = place(&stations, &[], &needs(&requires, &[]), 1, false).unwrap_err();
    assert_eq!(err, PlaceError::Unmet("gpu".to_owned()));
    assert_eq!(err.text(), "No station matches: gpu.");
    assert_eq!(
        PlaceError::NoneHasAll.text(),
        "No station matches: no station has every requirement."
    );
    assert_eq!(
        PlaceError::NoReadyHome.text(),
        "No station matches: the project has no ready home."
    );
}
