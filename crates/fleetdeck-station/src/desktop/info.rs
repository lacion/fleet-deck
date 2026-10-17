//! `DesktopInfo` (R13 §6.2): monitors from `hyprctl -j monitors`, the lock,
//! the do-not-disturb and the idle state, and the tools on the `PATH`.
//! "Unknown is never zero": a value that cannot be read is `None`.

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::desktop::{
    DesktopAllowed, DesktopInfo, DesktopKind, DesktopTools, MonitorInfo,
};
use serde_json::Value;

use super::detect::Found;
use super::quiet;
use super::run::{CALL_TIMEOUT, Runner, on_path};
use super::{Fail, code};

/// A monitor with the workspace that it shows now.
#[derive(Debug, Clone)]
pub struct RawMonitor {
    pub info: MonitorInfo,
    pub active_workspace: Option<i64>,
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_owned()).collect()
}

fn unreadable(program: &str) -> Fail {
    Fail::new(
        code::FAILED,
        format!("`{program}` gave an answer that cannot be read"),
    )
}

fn rounded(v: Option<f64>, times: f64) -> u32 {
    let n = (v.unwrap_or(0.0) * times).round();
    if n.is_finite() && n >= 0.0 {
        // The value is a small, rounded, non-negative number.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let n = n.min(f64::from(u32::MAX)) as u32;
        n
    } else {
        0
    }
}

fn parse_monitor(v: &Value) -> Option<RawMonitor> {
    let name = v.get("name")?.as_str()?.to_owned();
    let number = |key: &str| {
        v.get(key)
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0)
    };
    Some(RawMonitor {
        info: MonitorInfo {
            name,
            description: v
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            width: number("width"),
            height: number("height"),
            scale_pct: rounded(v.get("scale").and_then(Value::as_f64), 100.0),
            refresh_hz: rounded(v.get("refreshRate").and_then(Value::as_f64), 1.0),
            focused: v.get("focused").and_then(Value::as_bool).unwrap_or(false),
            dpms_on: v.get("dpmsStatus").and_then(Value::as_bool),
        },
        active_workspace: v
            .get("activeWorkspace")
            .and_then(|w| w.get("id"))
            .and_then(Value::as_i64),
    })
}

/// The live monitors.
///
/// # Errors
/// `failed` when `hyprctl` fails or its answer cannot be read.
pub async fn monitors(runner: &Runner<'_>) -> Result<Vec<RawMonitor>, Fail> {
    let out = runner
        .run_ok("hyprctl", &argv(&["-j", "monitors"]), CALL_TIMEOUT)
        .await?;
    let list: Value = serde_json::from_str(&out.stdout).map_err(|_| unreadable("hyprctl"))?;
    let list = list.as_array().ok_or_else(|| unreadable("hyprctl"))?;
    Ok(list.iter().filter_map(parse_monitor).collect())
}

/// The live windows as `(address, class)`.
///
/// # Errors
/// `failed` when `hyprctl` fails or its answer cannot be read.
pub async fn clients(runner: &Runner<'_>) -> Result<Vec<(String, String)>, Fail> {
    let out = runner
        .run_ok("hyprctl", &argv(&["-j", "clients"]), CALL_TIMEOUT)
        .await?;
    let list: Value = serde_json::from_str(&out.stdout).map_err(|_| unreadable("hyprctl"))?;
    let list = list.as_array().ok_or_else(|| unreadable("hyprctl"))?;
    Ok(list
        .iter()
        .filter_map(|c| {
            let address = c.get("address")?.as_str()?.to_ascii_lowercase();
            let class = c.get("class")?.as_str()?.to_owned();
            Some((address, class))
        })
        .collect())
}

/// Whether a process `name` of this user runs: `None` when `pgrep` is
/// missing or fails in another way than "no match".
async fn running(runner: &Runner<'_>, name: &str) -> Option<bool> {
    let uid = rustix::process::getuid().as_raw().to_string();
    let out = runner
        .run("pgrep", &argv(&["-u", &uid, "-x", name]), CALL_TIMEOUT)
        .await
        .ok()?;
    match out.code {
        0 => Some(true),
        1 => Some(false),
        _ => None,
    }
}

/// Omarchy turns idle off by stopping `hypridle`: inhibited when no
/// `hypridle` of this user runs. `None` when that cannot be told.
pub async fn idle_inhibited(runner: &Runner<'_>) -> Option<bool> {
    running(runner, "hypridle").await.map(|up| !up)
}

async fn dnd(runner: &Runner<'_>) -> Option<bool> {
    let out = runner
        .run("makoctl", &argv(&["mode"]), CALL_TIMEOUT)
        .await
        .ok()?;
    (out.code == 0).then(|| out.stdout.contains("do-not-disturb"))
}

/// The tools on the desktop `PATH`.
#[must_use]
pub fn tools(env: &ChildEnv) -> DesktopTools {
    DesktopTools {
        wtype: on_path(env, "wtype"),
        pointer: on_path(env, "ydotool") || on_path(env, "wlrctl"),
        recorder: on_path(env, "gpu-screen-recorder"),
        grim: on_path(env, "grim"),
        chromium: on_path(env, "chromium"),
        notify_send: on_path(env, "notify-send"),
    }
}

/// Reads the info of the desktop. `allowed` and `walls` are placeholders:
/// the caller sets the live values.
///
/// # Errors
/// `failed` when the monitors cannot be read.
pub async fn collect(env: &ChildEnv, found: &Found) -> Result<DesktopInfo, Fail> {
    let runner = Runner {
        env,
        gate: None,
        start: None,
    };
    let monitors = monitors(&runner)
        .await?
        .into_iter()
        .map(|m| m.info)
        .collect();
    let locked = running(&runner, "hyprlock").await;
    let idle_inhibited = if found.kind == DesktopKind::Omarchy {
        idle_inhibited(&runner).await
    } else {
        None
    };
    let offset = quiet::read_offset(env).await;
    let sampled_at = quiet::now_ms();
    Ok(DesktopInfo {
        kind: found.kind,
        omarchy_version: found.omarchy_version.clone(),
        monitors,
        locked,
        dnd: dnd(&runner).await,
        idle_inhibited,
        walls: Vec::new(),
        allowed: DesktopAllowed {
            desktop: false,
            capture: false,
            capture_monitors: Vec::new(),
            input: false,
        },
        tools: tools(env),
        // The UTC offset of the station (`date +%z`) and when it was read.
        utc_offset_minutes: offset,
        offset_sampled_at: offset.map(|_| sampled_at),
        features: Vec::new(),
    })
}
