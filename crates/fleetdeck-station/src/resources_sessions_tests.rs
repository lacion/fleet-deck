use super::*;

const TEN_S: Duration = Duration::from_secs(10);

/// One fixture process: pid, parent, comm, CPU ticks, start time, VmRSS in
/// kB (`None`: no `status` file).
fn put(root: &Path, pid: u32, ppid: u32, comm: &str, ticks: u64, start: u64, rss: Option<u64>) {
    let dir = root.join(pid.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    // The fields after the comm: state is field 3, so field n is `f[n - 3]`.
    let mut f = vec!["0".to_owned(); 30];
    f[0] = "S".to_owned();
    f[1] = ppid.to_string();
    f[11] = (ticks / 2).to_string();
    f[12] = (ticks - ticks / 2).to_string();
    f[19] = start.to_string();
    std::fs::write(
        dir.join("stat"),
        format!("{pid} ({comm}) {}\n", f.join(" ")),
    )
    .unwrap();
    if let Some(kb) = rss {
        std::fs::write(
            dir.join("status"),
            format!("Name:\tx\nVmRSS:\t{kb:>8} kB\nThreads:\t1\n"),
        )
        .unwrap();
    }
}

fn sid(s: &str) -> SessionId {
    s.into()
}

fn roots() -> Vec<(SessionId, u32)> {
    vec![(sid("a"), 100), (sid("b"), 200)]
}

/// Session `a`: 100 with children 101 and 102 and a grandchild 103 (no
/// `status` file, and a comm with `) (`). Session `b`: 200 with child 201.
/// Process 300 belongs to no session.
fn state_one(root: &Path) {
    put(root, 1, 0, "init", 9_999, 1, Some(10));
    put(root, 100, 1, "claude", 100, 10, Some(1000));
    put(root, 101, 100, "node", 50, 11, Some(2000));
    put(root, 102, 100, "a) (b", 10, 12, Some(3000));
    put(root, 103, 101, "sh", 40, 13, None);
    put(root, 200, 1, "codex", 10, 20, Some(100));
    put(root, 201, 200, "rg", 20, 21, Some(200));
    put(root, 300, 1, "other", 5_000, 30, Some(50_000));
    std::fs::create_dir_all(root.join("self")).unwrap();
}

#[test]
fn the_first_sample_has_sums_and_no_cpu() {
    let tmp = tempfile::tempdir().unwrap();
    state_one(tmp.path());
    let (loads, prev) = sample_tree(tmp.path(), &roots(), &Prev::default(), Duration::ZERO);
    assert_eq!(
        loads,
        vec![
            SessionLoad {
                session_id: sid("a"),
                cpu_pct: None,
                // 1000 + 2000 + 3000 kB; 103 has no status file.
                rss_bytes: Some(6000 * 1024),
                procs: 4,
            },
            SessionLoad {
                session_id: sid("b"),
                cpu_pct: None,
                rss_bytes: Some(300 * 1024),
                procs: 2,
            },
        ]
    );
    // Only the trees of the sessions are kept.
    assert_eq!(prev.ticks.len(), 6);
    assert!(!prev.ticks.contains_key(&(300, 30)));
}

#[test]
fn the_second_sample_counts_the_tick_delta_with_an_ended_and_a_new_child() {
    let tmp = tempfile::tempdir().unwrap();
    state_one(tmp.path());
    let (_, prev) = sample_tree(tmp.path(), &roots(), &Prev::default(), Duration::ZERO);

    // 100 +500 ticks, 101 +200, 103 +0, 102 ended, and a new process that
    // reuses pid 102 with a new start time and 300 ticks: 1000 ticks in
    // 10 s are 100% of one core. Session `b` did not run.
    std::fs::remove_dir_all(tmp.path().join("102")).unwrap();
    put(tmp.path(), 100, 1, "claude", 600, 10, Some(1000));
    put(tmp.path(), 101, 100, "node", 250, 11, Some(2000));
    put(tmp.path(), 102, 100, "new", 300, 99, Some(10));
    let (loads, next) = sample_tree(tmp.path(), &roots(), &prev, TEN_S);
    assert_eq!(loads[0].session_id, sid("a"));
    assert_eq!(loads[0].cpu_pct, Some(100));
    assert_eq!(loads[0].procs, 4);
    assert_eq!(loads[0].rss_bytes, Some(3010 * 1024));
    assert_eq!(loads[1].session_id, sid("b"));
    assert_eq!(loads[1].cpu_pct, Some(0));
    // The next state keeps the new key and drops the old pid 102.
    assert!(next.ticks.contains_key(&(102, 99)));
    assert!(!next.ticks.contains_key(&(102, 12)));
}

#[test]
fn a_whole_busy_machine_is_6400_percent() {
    let tmp = tempfile::tempdir().unwrap();
    put(tmp.path(), 100, 1, "x", 0, 10, Some(1));
    let r = [(sid("a"), 100)];
    let (_, prev) = sample_tree(tmp.path(), &r, &Prev::default(), Duration::ZERO);
    // 64 cores busy for 10 s: 64_000 ticks.
    put(tmp.path(), 100, 1, "x", 64_000, 10, Some(1));
    let (loads, _) = sample_tree(tmp.path(), &r, &prev, TEN_S);
    assert_eq!(loads[0].cpu_pct, Some(6400));
}

#[test]
fn a_comm_with_a_closing_parenthesis_and_spaces_parses() {
    let p = parse_stat("42 (a) (b c) S 7 0 0 0 0 0 0 0 0 0 30 12 0 0 0 0 0 0 555 0").unwrap();
    assert_eq!((p.ppid, p.ticks, p.start), (7, 42, 555));
    assert!(parse_stat("garbage").is_none());
}

#[test]
fn a_missing_root_has_no_rss_and_no_processes() {
    let tmp = tempfile::tempdir().unwrap();
    put(tmp.path(), 1, 0, "init", 1, 1, Some(1));
    let (loads, prev) = sample_tree(tmp.path(), &[(sid("gone"), 555)], &Prev::default(), TEN_S);
    assert_eq!(
        loads,
        vec![SessionLoad {
            session_id: sid("gone"),
            cpu_pct: None,
            rss_bytes: None,
            procs: 0,
        }]
    );
    assert!(prev.ticks.is_empty());
}

#[test]
fn a_missing_proc_root_gives_empty_trees() {
    let tmp = tempfile::tempdir().unwrap();
    let (loads, _) = sample_tree(
        &tmp.path().join("none"),
        &[(sid("a"), 100)],
        &Prev::default(),
        TEN_S,
    );
    assert_eq!(loads[0].procs, 0);
}

fn load(id: &str, cpu: Option<u32>) -> SessionLoad {
    SessionLoad {
        session_id: sid(id),
        cpu_pct: cpu,
        rss_bytes: None,
        procs: 1,
    }
}

#[test]
fn the_list_is_sorted_by_cpu_descending_none_last_then_by_id() {
    let mut loads = vec![
        load("d", None),
        load("c", Some(10)),
        load("b", Some(300)),
        load("a", None),
        load("e", Some(10)),
        load("f", Some(0)),
    ];
    sort_loads(&mut loads);
    let order: Vec<_> = loads.iter().map(|l| l.session_id.to_string()).collect();
    assert_eq!(order, ["b", "c", "e", "f", "a", "d"]);
}

#[test]
fn a_session_with_an_unknown_pid_keeps_its_last_pid_and_an_ended_one_is_dropped() {
    let mut known = BTreeMap::new();
    // Never seen and no pid: left out.
    let first = merge_roots(
        &mut known,
        vec![(sid("a"), Some(7)), (sid("b"), None), (sid("c"), Some(9))],
    );
    assert_eq!(first, vec![(sid("a"), 7), (sid("c"), 9)]);
    // `a` has a busy lock now: its last pid stays. `c` ended.
    let second = merge_roots(&mut known, vec![(sid("a"), None), (sid("b"), Some(8))]);
    assert_eq!(second, vec![(sid("a"), 7), (sid("b"), 8)]);
}
