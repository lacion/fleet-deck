//! Station resources in `station.info` and the standard limits (DL-193).
//!
//! The deck never computes warnings. The board applies the same rules, with
//! a TypeScript port of [`StationResources::warnings`]. A warning never
//! blocks a session start.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::SessionId;

/// A disk is low when it has less free space than this (decimal GB).
pub const LOW_DISK_BYTES: u64 = 10_000_000_000;
/// The memory is low when less than this is available (decimal GB).
pub const LOW_MEMORY_BYTES: u64 = 1_000_000_000;
/// A disk or the memory is also low below this share of its total, in percent.
pub const LOW_PERCENT: u64 = 10;
/// The CPU is busy when `load5` is above this, per core, times 100.
pub const BUSY_LOAD_X100_PER_CORE: u32 = 150;

/// What a station reports about its disk, memory and CPU (DL-193).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct StationResources {
    /// When the station measured it, in Unix ms.
    pub at: i64,
    pub disks: Vec<DiskSpace>,
    pub memory: Option<MemoryUse>,
    pub cpu: Option<CpuLoad>,
    /// The load of each live session's process tree, busiest first (R13 §10).
    /// Sorted by `cpu_pct` descending, `None` last, then by `session_id`.
    #[serde(default)]
    pub sessions: Vec<SessionLoad>,
}

/// The CPU, memory and process count of one live session's process tree
/// (R13 §10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct SessionLoad {
    pub session_id: SessionId,
    /// Percent of one core, rounded; 6400 is a whole 64-core machine.
    /// `None`: not measured yet or unreadable.
    pub cpu_pct: Option<u32>,
    pub rss_bytes: Option<u64>,
    pub procs: u32,
}

/// The space of one filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct DiskSpace {
    /// `Data` for the station data directory, else the allowed root path.
    pub label: String,
    pub path: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

/// The memory of the station host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct MemoryUse {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

/// The CPU load of the station host. The loads are the load average times
/// 100, rounded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct CpuLoad {
    pub cores: u32,
    pub load1_x100: u32,
    pub load5_x100: u32,
}

/// The part of the resources that a warning is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "link/")]
pub enum ResourceKind {
    Disk,
    Memory,
    Cpu,
}

/// One warning about low resources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct ResourceWarning {
    pub kind: ResourceKind,
    pub text: String,
}

/// A size as decimal GB with one decimal, for example `12.3 GB`.
fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

/// `free` is low when it is under `limit`, or under `LOW_PERCENT` of `total`.
/// A `total` of 0 is never low.
fn low(free: u64, total: u64, limit: u64) -> bool {
    total > 0
        && (free < limit || u128::from(free) * 100 < u128::from(total) * u128::from(LOW_PERCENT))
}

impl StationResources {
    /// The warnings: one per low disk in list order, then memory, then CPU.
    #[must_use]
    pub fn warnings(&self) -> Vec<ResourceWarning> {
        let mut out = Vec::new();
        for d in &self.disks {
            if low(d.free_bytes, d.total_bytes, LOW_DISK_BYTES) {
                out.push(ResourceWarning {
                    kind: ResourceKind::Disk,
                    text: format!("Low disk: {} free on {}", gb(d.free_bytes), d.label),
                });
            }
        }
        if let Some(m) = &self.memory
            && low(m.available_bytes, m.total_bytes, LOW_MEMORY_BYTES)
        {
            out.push(ResourceWarning {
                kind: ResourceKind::Memory,
                text: format!(
                    "Low memory: {} free of {}",
                    gb(m.available_bytes),
                    gb(m.total_bytes)
                ),
            });
        }
        if let Some(c) = &self.cpu
            && c.cores > 0
            && u64::from(c.load5_x100) > u64::from(BUSY_LOAD_X100_PER_CORE) * u64::from(c.cores)
        {
            out.push(ResourceWarning {
                kind: ResourceKind::Cpu,
                text: format!(
                    "Busy CPU: load {:.1} on {} cores",
                    f64::from(c.load5_x100) / 100.0,
                    c.cores
                ),
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(label: &str, total: u64, free: u64) -> DiskSpace {
        DiskSpace {
            label: label.to_owned(),
            path: format!("/{label}"),
            total_bytes: total,
            free_bytes: free,
        }
    }

    fn res(disks: Vec<DiskSpace>) -> StationResources {
        StationResources {
            at: 1,
            disks,
            memory: None,
            cpu: None,
            sessions: Vec::new(),
        }
    }

    #[test]
    fn a_disk_is_low_below_10_gb_or_10_percent() {
        // Exactly 10 GB free of 50 GB is not low.
        let t = 50_000_000_000;
        assert!(
            res(vec![disk("Data", t, LOW_DISK_BYTES)])
                .warnings()
                .is_empty()
        );
        // One byte less than 10 GB is low.
        let w = res(vec![disk("Data", t, LOW_DISK_BYTES - 1)]).warnings();
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].kind, ResourceKind::Disk);
        assert_eq!(w[0].text, "Low disk: 10.0 GB free on Data");
        // On a big disk the percent rule decides: exactly 10 % is not low.
        let t = 10_000_000_000_000;
        assert!(res(vec![disk("/w", t, t / 10)]).warnings().is_empty());
        assert_eq!(res(vec![disk("/w", t, t / 10 - 1)]).warnings().len(), 1);
        // Total 0: no warning.
        assert!(res(vec![disk("Data", 0, 0)]).warnings().is_empty());
    }

    #[test]
    fn the_memory_limits_are_1_gb_and_10_percent() {
        let mem = |total, available| StationResources {
            memory: Some(MemoryUse {
                total_bytes: total,
                available_bytes: available,
            }),
            ..res(Vec::new())
        };
        assert!(mem(16_000_000_000, 2_000_000_000).warnings().is_empty());
        assert!(mem(16_000_000_000, 1_600_000_000).warnings().is_empty());
        let w = mem(16_000_000_000, 1_599_999_999).warnings();
        assert_eq!(w[0].kind, ResourceKind::Memory);
        assert_eq!(w[0].text, "Low memory: 1.6 GB free of 16.0 GB");
        let w = mem(2_000_000_000, LOW_MEMORY_BYTES - 1).warnings();
        assert_eq!(w[0].text, "Low memory: 1.0 GB free of 2.0 GB");
        let w = mem(500_000_000, 400_000_000).warnings();
        assert_eq!(w[0].text, "Low memory: 0.4 GB free of 0.5 GB");
        assert!(mem(0, 0).warnings().is_empty());
    }

    #[test]
    fn the_cpu_is_busy_above_1_5_times_the_cores() {
        let cpu = |cores, load5_x100| StationResources {
            cpu: Some(CpuLoad {
                cores,
                load1_x100: 0,
                load5_x100,
            }),
            ..res(Vec::new())
        };
        assert!(cpu(4, 600).warnings().is_empty());
        let w = cpu(4, 601).warnings();
        assert_eq!(w[0].kind, ResourceKind::Cpu);
        assert_eq!(w[0].text, "Busy CPU: load 6.0 on 4 cores");
        assert!(cpu(0, 1000).warnings().is_empty());
    }

    #[test]
    fn the_warnings_follow_the_disks_then_memory_then_cpu() {
        let r = StationResources {
            at: 1,
            disks: vec![
                disk("Data", 100_000_000_000, 1),
                disk("/w", 100_000_000_000, 2),
            ],
            memory: Some(MemoryUse {
                total_bytes: 8_000_000_000,
                available_bytes: 1,
            }),
            cpu: Some(CpuLoad {
                cores: 2,
                load1_x100: 900,
                load5_x100: 900,
            }),
            sessions: Vec::new(),
        };
        let kinds: Vec<_> = r.warnings().iter().map(|w| w.kind).collect();
        assert_eq!(
            kinds,
            [
                ResourceKind::Disk,
                ResourceKind::Disk,
                ResourceKind::Memory,
                ResourceKind::Cpu
            ]
        );
    }

    #[test]
    fn a_station_info_without_resources_still_parses() {
        let old = serde_json::json!({
            "name": "n", "incarnation": "i", "head": 3, "live_sessions": 1
        });
        let info: crate::link::StationInfo = serde_json::from_value(old).unwrap();
        assert_eq!(info.resources, None);
        // `None` is not written, so an older deck sees the old shape.
        let back = serde_json::to_value(&info).unwrap();
        assert!(back.get("resources").is_none());
    }

    #[test]
    fn station_resources_round_trip_in_station_info() {
        let r = StationResources {
            at: 5,
            disks: vec![disk("Data", 10, 5)],
            memory: Some(MemoryUse {
                total_bytes: 2,
                available_bytes: 1,
            }),
            cpu: Some(CpuLoad {
                cores: 4,
                load1_x100: 12,
                load5_x100: 34,
            }),
            sessions: vec![SessionLoad {
                session_id: "s1".into(),
                cpu_pct: Some(340),
                rss_bytes: None,
                procs: 3,
            }],
        };
        let json = serde_json::json!({
            "name": "n", "incarnation": "i", "head": 3, "live_sessions": 1,
            "resources": r
        });
        let info: crate::link::StationInfo = serde_json::from_value(json).unwrap();
        assert_eq!(info.resources, Some(r));
    }

    #[test]
    fn resources_without_sessions_still_parse() {
        let old = serde_json::json!({"at": 1, "disks": [], "memory": null, "cpu": null});
        let r: StationResources = serde_json::from_value(old).unwrap();
        assert!(r.sessions.is_empty());
    }

    #[test]
    fn a_warning_kind_is_snake_case_on_the_wire() {
        let w = ResourceWarning {
            kind: ResourceKind::Memory,
            text: "x".to_owned(),
        };
        assert_eq!(
            serde_json::to_value(w).unwrap(),
            serde_json::json!({"kind": "memory", "text": "x"})
        );
    }
}
