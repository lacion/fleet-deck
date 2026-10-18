//! `fleetdeck omarchy install [--yes] [--screensaver]` (R13 P4 task C,
//! `omarchy-p4.md` §3.4 steps 1 to 7).

use std::io::{BufRead as _, Write as _};
use std::os::fd::AsFd as _;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{anyhow, bail};
use fleetdeck_core::paths::NodeDirs;
use fleetdeck_station::desktop::detect::omarchy_root;
use fleetdeck_station::desktop::run::on_path;

use super::assets::{Asset, Base, FILES, HYPR_PATH, MENU_PATH, menu_rows};
use super::fsx::Home;
use super::record::{self, FileEntry, Record, rel, sha256_hex};
use super::{TOOL_LIMIT, hyprland, omarchy_env, open_home, set_summary, tool};

const RESCAN: &str = "omarchy-shell shell rescanPlugins";
const ENABLE: &str = "omarchy plugin enable fleetdeck.fleet --section right";
const SUMMARY_ON: &str = "fleetdeck settings set --station fleet_summary true";

/// What the install does with one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plan {
    /// No file exists: write it.
    Create,
    /// An identical file exists that the record does not name: adopt it.
    Adopt,
    /// A recorded file that is as recorded: write the new content.
    Replace,
    /// A recorded file that changed: keep it.
    Keep,
    /// The menu file of the user exists: do not write it.
    SkipMenu,
    /// A file exists that is not recorded and differs.
    Conflict,
}

impl Plan {
    fn label(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Adopt => "adopt",
            Self::Replace => "replace",
            Self::Keep => "kept (changed)",
            Self::SkipMenu => "skip (exists)",
            Self::Conflict => "conflict",
        }
    }
}

struct Item {
    asset: &'static Asset,
    rel: PathBuf,
    abs: PathBuf,
    plan: Plan,
}

/// Step 3, the read-only part: decides what happens to each file. A link
/// or a foreign owner on a path is an error here, before any write.
fn classify(home: &Home, record: &Record) -> anyhow::Result<Vec<Item>> {
    let mut items = Vec::new();
    for asset in FILES {
        let rel = rel(asset.base, asset.path);
        let existing = home.read(&rel)?;
        let entry = record.entry(asset.base, asset.path);
        let plan = match (existing, entry) {
            (None, _) => Plan::Create,
            (Some(_), None) if asset.only_if_absent => Plan::SkipMenu,
            (Some(data), None) if data.bytes == asset.text.as_bytes() => Plan::Adopt,
            (Some(_), None) => Plan::Conflict,
            (Some(data), Some(entry)) if sha256_hex(&data.bytes) == entry.sha256 => Plan::Replace,
            (Some(_), Some(_)) => Plan::Keep,
        };
        items.push(Item {
            asset,
            abs: home.abs(&rel),
            rel,
            plan,
        });
    }
    Ok(items)
}

fn warn(text: &str) {
    eprintln!("fleetdeck: warning: {text}");
}

/// Step 2: the question on a terminal, or `--yes`.
fn confirm(yes: bool) -> anyhow::Result<()> {
    if yes {
        return Ok(());
    }
    let stdin = std::io::stdin();
    if !rustix::termios::isatty(stdin.as_fd()) {
        bail!("Run with --yes to confirm.");
    }
    print!("Continue? [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    stdin.lock().read_line(&mut answer)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        bail!("Cancelled; nothing was changed.")
    }
}

/// `fleetdeck omarchy install`.
///
/// # Errors
/// No Omarchy, a path that is a link or not yours, an unreadable record, a
/// failed write, or no confirmation.
pub async fn run(dirs: &NodeDirs, yes: bool, screensaver: bool) -> anyhow::Result<ExitCode> {
    // 1. Detect Omarchy. Compute the file set.
    let env = omarchy_env();
    let Some((root, version)) = omarchy_root(&env) else {
        bail!("Omarchy was not found on this machine.");
    };
    let home = open_home()?;
    let hypr_abs = home.abs(&rel(Base::Fleetdeck, HYPR_PATH));
    let line = hyprland::line_for(&hypr_abs).ok_or_else(|| {
        anyhow!("the config path holds characters that Lua cannot take; change nothing.")
    })?;
    let mut record = record::load(&home)?.unwrap_or_default();
    let items = classify(&home, &record)?;

    // 3 (first part). A conflict refuses before the question and before any write.
    let conflicts: Vec<&Item> = items.iter().filter(|i| i.plan == Plan::Conflict).collect();
    if !conflicts.is_empty() {
        for item in conflicts {
            eprintln!("fleetdeck: conflict: {}", item.abs.display());
        }
        return Ok(ExitCode::from(1));
    }

    // 2. Print the list. Ask.
    println!(
        "Omarchy {} found at {}.",
        version.as_deref().unwrap_or("(version unknown)"),
        root.display()
    );
    println!("Fleet Deck will write these files:");
    for item in &items {
        println!(
            "  {:04o}  {}  [{}]",
            item.asset.mode,
            item.abs.display(),
            item.plan.label()
        );
    }
    println!(
        "  0600  {}  [record]",
        home.abs(std::path::Path::new(record::RECORD_PATH))
            .display()
    );
    println!("It will also:");
    if !record.plugin_enabled {
        println!("  - run: {ENABLE}");
    }
    println!("  - turn on the station setting fleet_summary");
    println!(
        "  - add one managed line to {}",
        home.abs(std::path::Path::new(hyprland::FILE)).display()
    );
    if screensaver {
        println!("  - record that the screensaver is on");
    }
    confirm(yes)?;

    // 3. Write each file; the record follows each one.
    record.fleetdeck_version = fleetdeck_core::build::version().to_owned();
    for item in &items {
        let asset = item.asset;
        match item.plan {
            Plan::Create | Plan::Replace => {
                home.write(&item.rel, asset.text.as_bytes(), asset.mode)?;
                let existed_before = record
                    .entry(asset.base, asset.path)
                    .is_some_and(|e| e.existed_before);
                record.upsert(FileEntry {
                    path: asset.path.to_owned(),
                    base: asset.base,
                    sha256: sha256_hex(asset.text.as_bytes()),
                    existed_before,
                });
            }
            Plan::Adopt => record.upsert(FileEntry {
                path: asset.path.to_owned(),
                base: asset.base,
                sha256: sha256_hex(asset.text.as_bytes()),
                existed_before: true,
            }),
            Plan::Keep => println!("kept (changed): {}", item.abs.display()),
            Plan::SkipMenu | Plan::Conflict => {}
        }
        record.menu_file_written = record.entry(Base::Omarchy, MENU_PATH).is_some();
        record::save(&home, &record)?;
    }

    // 4. Rescan the plugins; enable the plugin once.
    let rescanned = matches!(
        tool(&env, "omarchy-shell", &["shell", "rescanPlugins"], TOOL_LIMIT).await,
        Ok(out) if out.code == 0 && out.stdout.trim() == "ok"
    );
    if !rescanned {
        warn(&format!("the plugins were not rescanned; run: {RESCAN}"));
    }
    if !record.plugin_enabled {
        let enabled = matches!(
            tool(
                &env,
                "omarchy",
                &["plugin", "enable", "fleetdeck.fleet", "--section", "right"],
                TOOL_LIMIT
            )
            .await,
            Ok(out) if out.code == 0
        );
        if enabled {
            record.plugin_enabled = true;
            record::save(&home, &record)?;
        } else {
            warn(&format!("the plugin is not enabled; run: {ENABLE}"));
        }
    }

    // 5. Turn on fleet_summary.
    match set_summary(dirs, true).await {
        Ok(changed) => {
            if changed {
                record.fleet_summary_set_by_install = true;
                record::save(&home, &record)?;
            }
        }
        Err(_) => warn(&format!(
            "the station is not running or did not accept the change, so fleet_summary stays off; run: {SUMMARY_ON}"
        )),
    }

    // 6. The screensaver switch (the job is task G).
    if screensaver {
        record.screensaver.enabled = true;
    }

    // 7. The managed Hyprland line, and what the user adds by hand.
    let done = hyprland::install(&home, &line, &record.hyprland_line);
    record.hyprland_line = done.record;
    record::save(&home, &record)?;
    println!("{}", done.message);
    if done.print_line {
        println!(
            "Add this line at the end of {}:",
            home.abs(std::path::Path::new(hyprland::FILE)).display()
        );
        println!("{line}");
    }
    if !record.menu_file_written {
        println!(
            "Add these rows inside the object of {}:",
            home.abs(&rel(Base::Omarchy, MENU_PATH)).display()
        );
        for row in menu_rows() {
            println!("{row}");
        }
    }
    if !on_path(&env, "fleetdeck") {
        warn("fleetdeck is not on PATH; the hotkeys and the hooks will not find it");
    }
    println!("Fleet Deck is installed for Omarchy.");
    Ok(ExitCode::SUCCESS)
}
